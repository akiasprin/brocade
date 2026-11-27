#!/usr/bin/env python3
"""Package and install the optional, node-only CVM stack (Python stdlib only).

The archive contains immutable runtime bytes and public configuration templates.
Disks, Cubelet metadata, execution records and administrator network settings stay
outside releases. A failed or interrupted activation restores the previous release.
"""

import argparse
import ctypes
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
from fnmatch import fnmatchcase
import gzip
import hashlib
import io
import importlib.util
import ipaddress
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shlex
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
FRESH_INSTALL_CONTRACT = 'brocade-cvm-stack-fresh-install-v1'
STACK_BUNDLE_MAX_BYTES = 8 * 1024**3
INSPECTION_ADDRESS = '127.0.0.1:18080'
INSPECTION_URL = f'http://{INSPECTION_ADDRESS}/v1/network/taps'
CUBELET_SOCKET = '/run/cubelet/cubelet.sock'
STATE_ABI = 'protected-root-v1+cubevs-attachments-v2'
BASE = Path('/opt/brocade/cvm')
TOOLBOX = Path('/usr/local/services/cubetoolbox')
STATE = Path('/data/cubelet')
PROC = Path('/proc')
HOST_BOOT_ID = PROC / 'sys/kernel/random/boot_id'
UNITS = ('brocade-cvm-cubelet.service',)
AGENT_UNIT = 'brocade-agent.service'
STACK_MARKER = 'stack-maintenance'
STACK_CHECKPOINT = 'stack-upgrade.json'
STACK_INSTALL_CHECKPOINT = 'stack-install.json'
STACK_INSTALL_ACTION = 'stack-install-activate'
BUNDLE_RECEIPT = '.bundle-identity.json'
FRESH_CONFIG_RECEIPT = '.fresh-install-configuration.json'
EXECUTOR_STATE = Path('/var/lib/brocade/cvm-cube')
CVM_CONFIG_DIR = Path('/etc/brocade')
AGENT_STATE = Path('/var/lib/brocade-agent')
FRESH_FOREIGN_PATHS = tuple(
    map(
        Path,
        (
            '/etc/cube',
            '/etc/cubelet',
            '/opt/cube',
            '/opt/cube-sandbox',
            '/var/lib/cubelet',
            '/usr/local/bin/cubelet',
            '/run/cube-sandbox-systemd',
            '/etc/modules-load.d/brocade-cvm.conf',
            '/etc/grub.d/42_brocade_cvm',
            '/etc/default/grub.d/99-brocade-cvm.cfg',
            '/var/lib/brocade/cvm-host',
        ),
    )
)
CUBELET_RUN = Path(CUBELET_SOCKET).parent
CUBELET_PID = Path('/run/cube-let.pid')
RUNTIME_DIRECTORIES = tuple(
    map(
        Path,
        (
            '/data/log/Cubelet',
            '/data/log/CubeShim',
            '/data/log/CubeVmm',
            '/data/cube-shim/disks',
            '/data/snapshot_pack/disks',
            '/data/cube-shared/volume',
        ),
    )
)
CUBELET = 'runtime/Cubelet/bin/cubelet'
SHIM = 'runtime/cube-shim/bin/containerd-shim-cube-rs'
AGENT_BIN = Path(os.environ.get('BROCADE_CVM_AGENT_BIN', '/usr/local/bin/brocade-agent'))
MANAGER = Path('/usr/local/lib/brocade/cvm-stack.py')
REQUIRED = (
    'runtime/Cubelet/bin/cubelet',
    'runtime/cube-shim/bin/containerd-shim-cube-rs',
    'runtime/cube-shim/bin/cube-runtime',
    'runtime/cube-kernel-scf/vmlinux',
    'runtime/cube-image/cube-guest-image-cpu.img',
    'runtime/cube-agent/cube-agent.ext4',
    'runtime/cube-shim/version',
    'runtime/cube-kernel-scf/version',
    'runtime/cube-image/version',
    'runtime/cube-agent/version',
    'config/cubelet.toml',
    'config/dynamic.yaml',
    'config/cube.json',
    'config/network.json',
    'runtime/bin/cvm-host.py',
    'runtime/bin/cvm-stack.py',
)
MAX_FILES = 4096
MAX_TOTAL = 16 * 1024**3
MAX_BUNDLE = MAX_TOTAL + 64 * 1024**2
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
        print(f'[{timestamp()}] CVM {self.stage}: {time.monotonic() - self.started:.1f}s', file=sys.stderr, flush=True)

    def report_periodically(self):
        while not self.stop.wait(30):
            self.report()

    def finish(self, status):
        self.stop.set()
        self.worker.join(timeout=1)
        print(
            f'[{timestamp()}] CVM {status}, elapsed {time.monotonic() - self.started:.3f}s', file=sys.stderr, flush=True
        )


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


def stream_digest(stream):
    h = hashlib.sha256()
    for block in iter(lambda: stream.read(1024**2), b''):
        h.update(block)
    return h.hexdigest()


def digest(path):
    with path.open('rb') as stream:
        return stream_digest(stream)


def is_sha256(value):
    return isinstance(value, str) and re.fullmatch(r'[0-9a-f]{64}', value) is not None


def owned(path, kind=stat.S_ISREG):
    info = path.lstat()
    if not kind(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
        raise StackError(f'CVM {path.name} is not installer-owned')
    return info


def owned_json(path):
    owned(path)
    value = read_json(path)
    if not isinstance(value, dict):
        raise StackError(f'invalid CVM {path.name}')
    return value


def present(path):
    return path.exists() or path.is_symlink()


def safe_name(name):
    p = PurePosixPath(name)
    return (
        bool(name)
        and not p.is_absolute()
        and str(p) == name
        and all(part not in ('', '.', '..') and re.fullmatch(r'[A-Za-z0-9_.+-]+', part) for part in p.parts)
    )


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
    if (
        not isinstance(backends, list)
        or not backends
        or any(not isinstance(b, str) or b not in ('xfs', 'plain') for b in backends)
        or len(backends) != len(set(backends))
    ):
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
        if not is_sha256(item.get('sha256')):
            raise StackError('invalid stack file digest')
        size = item.get('bytes')
        if type(size) is not int or size < 0 or size > MAX_TOTAL:
            raise StackError('invalid stack file size')
        if item.get('mode') not in (0o600, 0o644, 0o755):
            raise StackError('invalid stack file permissions')
        total += size
    if total > MAX_TOTAL or any(name not in files for name in REQUIRED):
        raise StackError('incomplete or oversized node runtime')
    if not is_sha256(value.get('source_sha256')):
        raise StackError('stack source identity is missing')
    floor = value.get('min_glibc')
    if floor is not None and (not isinstance(floor, str) or not re.fullmatch(r'[0-9]{1,4}\.[0-9]{1,4}', floor)):
        raise StackError('invalid glibc compatibility floor')
    return value


def inspect_archive(path, expected, destination=None):
    # Hash and read through the same descriptor so a renamed upload path cannot
    # substitute different bytes between verification and extraction.
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise StackError('CVM bundle must be an owned regular file without group or other write access')
        if info.st_size > MAX_BUNDLE:
            raise StackError('CVM bundle exceeds size limit')
        if not is_sha256(expected) or stream_digest(stream) != expected:
            raise StackError('CVM bundle SHA-256 mismatch')
        stream.seek(0)
        manifest = read_archive(stream, destination)
        after = os.fstat(stream.fileno())
        if (info.st_size, info.st_mtime_ns, info.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise StackError('CVM bundle changed during verification')
    return manifest


def read_archive(stream, destination):
    with tarfile.open(fileobj=stream, mode='r:gz') as archive:
        members = []
        total = 0
        for member in archive:
            if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE) or not safe_name(member.name):
                raise StackError('archive contains a link, device or unsafe path')
            total += member.size
            if member.size < 0 or total > MAX_TOTAL + MAX_JSON or len(members) >= MAX_FILES + 1:
                raise StackError('CVM archive exceeds member or payload limits')
            members.append(member)
        names = [member.name for member in members]
        if len(set(names)) != len(names) or 'manifest.json' not in names:
            raise StackError('duplicate archive member or missing manifest')
        manifest_member = archive.getmember('manifest.json')
        if manifest_member.size > MAX_JSON:
            raise StackError('stack manifest exceeds size limit')
        manifest = validate_manifest(json.load(archive.extractfile(manifest_member)))
        if set(names) != {'manifest.json', *manifest['files']}:
            raise StackError('archive differs from its manifest inventory')
        if destination and shutil.disk_usage(destination).free < sum(
            item['bytes'] for item in manifest['files'].values()
        ):
            raise StackError('insufficient space for a staged CVM release; the active release is unchanged')
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
    if binding.get('backend') != backend or (
        backend == 'plain' and binding.get('filesystem_uuid') != local_storage_uuid()
    ):
        raise StackError('CVM storage filesystem changed or is not mounted; refusing a replacement pool')


def require_storage_support(manifest, backend):
    backends = manifest.get('storage_backends')
    if not isinstance(backends, list) or backend not in backends:
        raise StackError('this CVM runtime does not support the existing storage backend; keep the current release')


def host_compatibility(manifest, check_virtualization=True):
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


def preflight(manifest, check_virtualization=True):
    host_compatibility(manifest, check_virtualization)
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
    probe_storage(storage, backend)


def probe_storage(storage, backend):
    # Probe the actual filesystem without leaving named files in the pool.
    with tempfile.TemporaryFile(dir=storage) as target:
        if backend == 'xfs':
            with tempfile.TemporaryFile(dir=storage) as source:
                source.write(b'CVM reflink preflight\n')
                source.flush()
                fcntl.ioctl(target.fileno(), 0x40049409, source.fileno())
        else:
            os.posix_fallocate(target.fileno(), 0, 1024**2)
            target.write(b'CVM plain allocation preflight\n')
        target.flush()
        os.fsync(target.fileno())
        if backend == 'plain' and os.fstat(target.fileno()).st_blocks * 512 < 1024**2:
            raise StackError('ext4 storage did not retain the requested physical allocation')


def lstat_if_present(path):
    # Path.exists() can collapse inaccessible or malformed paths into False.
    try:
        return path.lstat()
    except FileNotFoundError:
        return None


def absent(path):
    if lstat_if_present(path) is not None:
        raise StackError(f'existing CVM {path.name} prevents fresh installation')


def existing_directory(path):
    """Return the nearest existing directory, without following untrusted parents."""
    existing = Path(path.anchor)
    for directory in (*reversed(path.parents), path):
        if lstat_if_present(directory) is None:
            break
        owned(directory, stat.S_ISDIR)
        existing = directory
    return existing


def empty_directory(path, *, lost_found=False):
    if lstat_if_present(path) is None:
        return
    owned(path, stat.S_ISDIR)
    for entry in path.iterdir():
        if lost_found and entry.name == 'lost+found':
            empty_directory(entry)
        else:
            raise StackError(f'existing CVM contents in {path.name} prevent fresh installation')


def fresh_native_host():
    host_compatibility({'architecture': 'x86_64'}, check_virtualization=False)
    if 'pvm.host' in platform.release() or lstat_if_present(Path('/sys/module/kvm_pvm')) is not None:
        raise StackError('CVM fresh installation requires native KVM')
    # The API version alone is insufficient on nested or disabled KVM hosts.
    with open('/dev/kvm', 'r+b', buffering=0) as device:
        if fcntl.ioctl(device.fileno(), 0xAE00, 0) != 12:
            raise StackError('unsupported KVM API')
        os.close(fcntl.ioctl(device.fileno(), 0xAE01, 0))


def fresh_storage(*, probe=False):
    storage_path = existing_directory(STATE / 'storage')
    mounts = []
    for path in (storage_path, existing_directory(STATE / 'root'), existing_directory(STATE / 'state')):
        fields = run(['findmnt', '-n', '-o', 'FSTYPE,UUID,OPTIONS', '-T', str(path)]).split()
        if (
            len(fields) != 3
            or fields[0] not in ('xfs', 'ext4')
            or not re.fullmatch(r'[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}', fields[1])
            or 'rw' not in fields[2].split(',')
        ):
            raise StackError('CVM fresh installation requires writable persistent XFS or ext4 filesystems')
        mounts.append(fields[:2])
    backend = 'xfs' if mounts[0][0] == 'xfs' else 'plain'
    if probe:
        probe_storage(storage_path, backend)
    return {'backend': backend, 'filesystems': mounts}


def empty_roots_digest(backend):
    return hashlib.sha256(
        json.dumps(['brocade-cvm-cold-roots-v1', backend, []], separators=(',', ':')).encode()
    ).hexdigest()


def fresh_cube_units(allowed=()):
    patterns = [
        '*cube*.service',
        '*cube*.socket',
        '*cube*.target',
        'brocade-cvm-*.socket',
        'brocade-cvm-resume.service',
        'brocade-host-terminal.service',
        'brocade-host-system.service',
    ]
    # Some systemd versions return exit 1 for a successful glob search with no matches.
    # Enumerate without globs so a genuine listing failure still blocks installation.
    common = ['--no-legend', '--no-pager', '--plain']
    for command in (['list-unit-files', *common], ['list-units', '--all', *common]):
        for line in run(['systemctl', *command]).splitlines():
            unit, *_ = line.split()
            if unit not in allowed and any(fnmatchcase(unit, pattern) for pattern in patterns):
                raise StackError('an installed or loaded Cube unit prevents fresh CVM installation')


def fresh_no_cube_processes():
    for process in PROC.iterdir():
        if not process.name.isascii() or not process.name.isdecimal():
            continue
        try:
            name = (process / 'comm').read_text().strip().lower()
            if name == 'containerd-shim':  # Linux truncates comm to 15 bytes.
                name = Path(os.readlink(process / 'exe')).name.lower()
        except (FileNotFoundError, ProcessLookupError):
            continue
        if re.match(r'^(?:cubelet|cube-let|cubevs|cube-(?:vmm|shim|runtime)|containerd-shim-cube)', name):
            raise StackError('an existing Cube process prevents fresh CVM installation')
    sockets = run(['ss', '-H', '-x', '-a', '-n'])
    if re.search(r'(?i)(?:[/@][^\s]*cube[^\s]*(?:\.sock|/)|[/@]cube[-_]?let\b)', sockets):
        raise StackError('an existing Cube socket prevents fresh CVM installation')


def fresh_entry_records(path):
    owned(path, stat.S_ISDIR)
    empty = {
        'rules.nft': b'add table ip brocade_cvm_entries\ndelete table ip brocade_cvm_entries\n',
        'phantun.nft': b'add table inet brocade_entry_phantun\ndelete table inet brocade_entry_phantun\n',
        'tcp-transports.json': b'{"ports":[],"targets":[]}',
        'wireguard-ports.json': b'[]',
    }
    # These are the Agent's exact empty outputs. Unknown children can hold tunnel state.
    for entry in path.iterdir():
        expected = empty.get(entry.name)
        if expected is None or owned(entry).st_size != len(expected) or entry.read_bytes() != expected:
            return False
    return True


def fresh_network_diagnostics(path):
    value = owned_json(path)
    return (
        set(value) == {'collected_at', 'dropped_events', 'events', 'resources'}
        and type(value['collected_at']) is int
        and value['collected_at'] >= 0
        and type(value['dropped_events']) is int
        and value['dropped_events'] >= 0
        and value['resources'] == []
        and isinstance(value['events'], list)
        and len(value['events']) <= 256
        and all(
            isinstance(event, dict)
            and set(event) == {'at', 'resource_id', 'source', 'level', 'message'}
            and event['resource_id'] is None
            and event['source'] == 'agent'
            and event['level'] in ('info', 'warning', 'error')
            and type(event['at']) is int
            and event['at'] >= 0
            and isinstance(event['message'], str)
            and len(event['message'].encode()) <= 1024
            for event in value['events']
        )
    )


def fresh_agent_records(allowed_cvm):
    if run(['systemctl', 'show', '--value', '-p', 'LoadState', AGENT_UNIT]) != 'loaded':
        raise StackError('CVM fresh installation requires the local Brocade Agent service')
    environment = shlex.split(run(['systemctl', 'show', '--value', '-p', 'Environment', AGENT_UNIT]))
    envfiles = run(['systemctl', 'show', '--value', '-p', 'EnvironmentFiles', AGENT_UNIT])
    for line in envfiles.splitlines():
        match = re.fullmatch(r'(/[^\s*?\[\]]+) \(ignore_errors=(yes|no)\)', line)
        if not match:
            raise StackError('cannot verify the Brocade Agent environment files')
        path = Path(match[1])
        existing_directory(path.parent)
        if lstat_if_present(path) is None and match[2] == 'yes':
            continue
        if owned(path).st_size > MAX_JSON:
            raise StackError('Brocade Agent environment file exceeds inspection limit')
        # Inspect only relevant assignments, never render environment contents in an error.
        for assignment in path.read_text().splitlines():
            if re.match(r'^\s*(?:BROCADE_CVM_\w+|BROCADE_AGENT_STATE_DIR)\s*=', assignment):
                fields = shlex.split(assignment.strip(), comments=False)
                if len(fields) != 1:
                    raise StackError('cannot verify the Brocade Agent CVM environment')
                environment.extend(fields)
    if re.search(r'\s--state-dir(?:\s|=)', run(['systemctl', 'show', '--value', '-p', 'ExecStart', AGENT_UNIT])):
        raise StackError('cannot verify a CLI override of the Brocade Agent state directory')
    states = {AGENT_STATE}
    for assignment in environment:
        name, _, value = assignment.partition('=')
        if name == 'BROCADE_AGENT_STATE_DIR':
            path = Path(value)
            if not path.is_absolute() or '..' in path.parts:
                raise StackError('cannot verify the Brocade Agent state directory')
            states.add(path)
        elif name.startswith('BROCADE_CVM_') and not name.startswith('BROCADE_CVM_STACK_'):
            if allowed_cvm.get(name) != value:
                raise StackError('an existing Brocade Agent CVM configuration prevents fresh installation')
    for directory in states:
        if existing_directory(directory) != directory:
            continue
        for path in directory.iterdir():
            if not path.name.startswith('cvm-'):
                continue
            # A CVM-capable Agent initializes networking even before Cube exists.
            # Only its node identity and explicitly resource-empty records are benign.
            if path.name == 'cvm-entries':
                if fresh_entry_records(path):
                    continue
            elif path.name == 'cvm-entry-flow-cleanup.json':
                value = owned_json(path)
                if value == {'intent': [], 'pending': [], 'completed': 0} and type(value['completed']) is int:
                    continue
            elif path.name == 'cvm-network-diagnostics.json':
                if fresh_network_diagnostics(path):
                    continue
            elif path.name == 'cvm-nic-identity.json':
                info = owned(path)
                if info.st_mode & 0o077 == 0 and info.st_size <= 32768:
                    value = owned_json(path)
                    if set(value) == {'certificate', 'key'} and all(
                        isinstance(value[field], str)
                        and value[field].startswith(f'-----BEGIN {label}-----\n')
                        and value[field].rstrip().endswith(f'-----END {label}-----')
                        for field, label in (('certificate', 'CERTIFICATE'), ('key', 'PRIVATE KEY'))
                    ):
                        continue
            elif path.name == 'cvm-nics':
                owned(path, stat.S_ISDIR)
                if all(entry.name == 'controller.lock' and owned(entry).st_size == 0 for entry in path.iterdir()):
                    continue
            elif path.name == 'cvm-nic-authority.sha256':
                if owned(path).st_size == 64 and is_sha256(path.read_text()):
                    continue
            elif path.name in ('cvm-nics-desired.json', 'cvm-entries-desired.json'):
                value = owned_json(path)
                if path.name == 'cvm-nics-desired.json':
                    # The Console includes management address exclusions even on
                    # hosts with no NIC bindings. They do not reserve resources.
                    networks = value.pop('management_networks', None)
                    if not isinstance(networks, list) or len(networks) > 2048:
                        raise StackError('invalid Brocade Agent CVM management networks')
                    try:
                        for network in networks:
                            if not isinstance(network, str) or '/' not in network:
                                raise ValueError
                            ipaddress.ip_network(network)
                    except ValueError:
                        raise StackError('invalid Brocade Agent CVM management networks') from None
                    empty = {'bindings': [], 'fences': [], 'budgets': {}}
                else:
                    empty = {'rules': [], 'deliveries': [], 'tunnels': []}
                if (
                    set(value) == {'node_id', 'generation', *empty}
                    and isinstance(value['node_id'], str)
                    and type(value['generation']) is int
                    and value['generation'] >= 0
                    and all(value[key] == item for key, item in empty.items())
                ):
                    continue
            raise StackError('existing Brocade Agent CVM records prevent fresh installation')


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
    for path in RUNTIME_DIRECTORIES:
        durable_mkdir(path)
        info = path.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise StackError('runtime directory ownership or permissions are unsafe')


def ready(timeout=60, config=None):
    deadline = time.monotonic() + timeout
    config = config or BASE / 'current/generated/cube.json'
    while time.monotonic() < deadline:
        try:
            result = subprocess.run(
                [str(AGENT_BIN), 'cvm-check', '--config', str(config)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=min(35, max(0.1, deadline - time.monotonic())),
            )
            if result.returncode == 0 and len(result.stdout) <= MAX_JSON:
                report = json.loads(result.stdout)
                caps = report.get('capabilities', {})
                if (
                    report.get('executor') == 'brocade-cube-rust-v1'
                    and caps.get('create')
                    and caps.get('stable_storage')
                ):
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
        return [
            info.st_dev,
            info.st_ino,
            info.st_size,
            info.st_mtime_ns,
            info.st_ctime_ns,
            info.st_mode,
            info.st_uid,
            info.st_gid,
            info.st_nlink,
        ]

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
        if (
            files[name][2] != item['bytes']
            or digest(path) != item['sha256']
            or fingerprint(path, item['mode']) != files[name]
        ):
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
    reserve = max(384 * mib, physical // 8)
    return max(0, physical - reserve) // (64 * mib) * (64 * mib)


def fresh_config_receipt(generated, job):
    files = {}
    remaining = [generated]
    entries = 0
    while remaining:
        directory = remaining.pop()
        owned(directory, stat.S_ISDIR)
        for path in directory.iterdir():
            if path == generated / FRESH_CONFIG_RECEIPT:
                continue
            entries += 1
            if entries > MAX_FILES:
                raise StackError('generated CVM configuration inventory exceeds limit')
            if stat.S_ISDIR(path.lstat().st_mode):
                remaining.append(path)
            elif owned(path).st_size > MAX_JSON:
                raise StackError('generated CVM configuration exceeds size limit')
            else:
                files[str(path.relative_to(generated))] = digest(path)
    return {'job': job, 'files': files}


def verify_fresh_config(release, job):
    generated = release / 'generated'
    owned(generated, stat.S_ISDIR)
    if owned_json(generated / FRESH_CONFIG_RECEIPT) != fresh_config_receipt(generated, job):
        raise StackError('generated CVM configuration differs from the recorded first installation')


def render_configuration(release, previous=None, mode='kvm', *, install_job=None):
    """Publish only a complete configuration, including when regenerating this release."""
    with tempfile.TemporaryDirectory(prefix='.generated-', dir=release) as temporary:
        generated = Path(temporary) / 'configuration'
        render_configuration_into(release, previous, mode, generated)
        if install_job is not None:
            atomic_json(generated / FRESH_CONFIG_RECEIPT, fresh_config_receipt(generated, install_job))
        destination = release / 'generated'
        if destination.exists():
            if destination.is_symlink() or not destination.is_dir():
                raise StackError('generated configuration is not an owned directory')
            # Linux rename exchange keeps the old complete directory available until
            # the new one is visible. A killed renderer never publishes its templates.
            libc = ctypes.CDLL(None, use_errno=True)
            if not hasattr(libc, 'renameat2'):
                raise StackError('atomic CVM configuration replacement requires Linux renameat2')
            exchange = libc.renameat2
            exchange.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
            exchange.restype = ctypes.c_int
            if exchange(-100, os.fsencode(generated), -100, os.fsencode(destination), 2) != 0:
                error = ctypes.get_errno()
                raise OSError(error, os.strerror(error))
        else:
            os.rename(generated, destination)
        sync_dir(release)


def render_configuration_into(release, previous, mode, generated):
    """Keep immutable payloads separate from host-rendered public configuration."""
    published = release / 'generated'
    if previous:
        old = read_json(previous / 'generated/cube.json')
        old_native = read_json(previous / 'generated/network.json')
        if 'bridge_sources' in old_native or old_native.get('state_dir') != '/data/cubelet/network-attachments-v2':
            raise StackError('obsolete CVM network configuration is unsupported; clean up the old runtime first')
        sources = old['capabilities']['network_sources']
        if len(sources) != 1 or sources[0]['kind'] != 'public':
            raise StackError('obsolete CVM local network sources are unsupported')
        if old.get('virtualization_mode') not in ('kvm', 'pvm'):
            raise StackError('retained CVM configuration requires an explicit virtualization mode')
        old_cubelet = (previous / 'generated/cubelet.toml').read_text()
        if (
            old.get('network_inspection_url') != INSPECTION_URL
            or tomllib.loads(old_cubelet)['http']['address'] != INSPECTION_ADDRESS
        ):
            raise StackError('obsolete CVM inspection configuration is unsupported')
        old_dynamic = (previous / 'generated/dynamic.yaml').read_bytes()
    retained = release / 'generated'
    if retained.exists():
        shutil.copytree(retained, generated, symlinks=True)
    else:
        generated.mkdir(mode=0o700)
    uplinks = json.loads(run(['ip', '-j', 'route', 'show', 'default']))
    uplink = next((route.get('dev') for route in uplinks if route.get('dev')), '')
    if not re.fullmatch(r'[A-Za-z0-9_.-]{1,15}', uplink):
        raise StackError('CVM requires an explicit usable IPv4 uplink')
    for source in sorted((release / 'config').rglob('*')):
        if source.is_file():
            data = (
                source.read_bytes()
                .replace(b'@CVM_RELEASE@', str(release).encode())
                .replace(b'@CVM_UPLINK@', uplink.encode())
            )
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
        # Preserve current administrator settings; executable artifacts come from this release.
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
        if memory < 128 * mib or disk_budget < 3 * gib:
            raise StackError('host capacity is below the minimum instance requirements')
        caps.update(allocatable_vcpus=cpu, allocatable_memory_bytes=memory, allocatable_disk_bytes=disk_budget)
        caps['resource_limits'] = dict(
            min_vcpus=1,
            max_vcpus=8,
            min_memory_bytes=128 * mib,
            max_memory_bytes=4 * gib,
            memory_step_bytes=64 * mib,
            min_root_disk_bytes=3 * gib,
            max_root_disk_bytes=64 * gib,
            root_disk_step_bytes=gib,
        )
    memory_budget = host_memory_budget()
    if memory_budget < 128 * 1024**2:
        raise StackError('host has insufficient memory after the runtime reserve')
    caps['allocatable_memory_bytes'] = min(caps['allocatable_memory_bytes'], memory_budget)
    config_path = generated / 'cubelet.toml'
    source = apply_resource_defaults(config_path.read_text(), (release / 'config/cubelet.toml').read_text())
    # The private inspection listener is owned by this runtime configuration.
    source = set_toml_values(source, 'http', {'address': INSPECTION_ADDRESS})
    cfg['network_inspection_url'] = INSPECTION_URL
    atomic(config_path, source.encode())
    dynamic_path = generated / 'dynamic.yaml'
    dynamic = dynamic_path.read_text()
    for key, value in [('creation_concurrent_num', 1), ('node_status_max_images', 256)]:
        dynamic = re.sub(r'(?m)^(\s*' + key + r'\s*:)\s*[^\n]*$', lambda match: match[1] + ' ' + str(value), dynamic)
    atomic(dynamic_path, dynamic.encode())
    # Recompute manifests after rendering paths; a system image remains independent of capacity.
    for image in caps['images']:
        path = Path(cfg['image_files'][image['id']])
        if not path.is_relative_to(published):
            raise StackError('packaged system manifest is outside the managed release configuration')
        image['sha256'] = digest(generated / path.relative_to(published))
    if previous:
        by_id = {image['id']: image for image in caps['images']}
        for image in old['capabilities']['images'] if old['virtualization_mode'] == mode else []:
            if image['id'] in by_id and image != by_id[image['id']]:
                raise StackError('a published image identity was changed by the CVM bundle')
            by_id[image['id']] = image
            cfg['image_files'].setdefault(image['id'], old['image_files'][image['id']])
        caps['images'] = [by_id[key] for key in sorted(by_id)]
    if 'bridge_sources' in native:
        raise StackError('CVM bundles must use Store-assigned NICs, without a local bridge catalogue')
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
    body = '\n'.join(
        line for line in body.splitlines() if not re.match(r'^\s*(image_base_path|shared_kernel_path)\s*=', line)
    )
    kernel = release / 'runtime' / ('cube-kernel-pvm' if mode == 'pvm' else 'cube-kernel-scf') / 'vmlinux'
    settings = (
        f'\n    image_base_path = {json.dumps(str(image_base))}\n    shared_kernel_path = {json.dumps(str(kernel))}\n'
    )
    atomic(config_path, (before + section + settings + body + separator + after).encode())
    configuration = tomllib.loads(config_path.read_text())
    if configuration.get('root') != str(STATE / 'root') or configuration.get('state') != str(STATE / 'state'):
        raise StackError('Cubelet metadata must use the persistent CVM data paths')
    if configuration['grpc']['address'] != cfg['cubelet_socket'] or configuration['grpc'].get('tcp_address'):
        raise StackError('Cubelet RPC must use the private configured Unix socket')
    if configuration['http']['address'] != INSPECTION_ADDRESS or configuration['debug']['address'] != '127.0.0.1:9966':
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


def fresh_component_inventory(release, manifest):
    files = manifest['files']
    inventory = {}
    for component in ('cube-shim', 'cube-kernel-scf', 'cube-image', 'cube-agent', 'cube-kernel-pvm'):
        prefix = f'runtime/{component}/'
        if not any(name.startswith(prefix) for name in files):
            continue
        version_file = release / prefix / 'version'
        if owned(version_file).st_size > 97:
            raise StackError('invalid immutable CVM component version')
        version = version_file.read_text().strip()
        if not re.fullmatch(r'[A-Za-z0-9_.-]{1,96}', version) or version in ('.', '..'):
            raise StackError('invalid immutable CVM component version')
        directory = 'cube-kernel-scf' if component == 'cube-kernel-pvm' else component
        for name, data in files.items():
            if not name.startswith(prefix) or (component == 'cube-kernel-pvm' and name != prefix + 'vmlinux'):
                continue
            target = STATE / 'root/component_versions' / directory / version / name.removeprefix(prefix)
            if target in inventory:
                raise StackError('conflicting immutable CVM component versions')
            inventory[target] = data
    prefix = 'runtime/cubebox_os_image/'
    for name, data in files.items():
        if name.startswith(prefix) and name.endswith('.ext4'):
            inventory[STATE / 'system-images' / name.removeprefix(prefix)] = data
    return inventory


def verify_fresh_components(release, manifest):
    # Before Cubelet has started only a subset of the approved immutable files
    # may exist. Do not adopt an opaque database, workspace or guest disk.
    inventory = fresh_component_inventory(release, manifest)
    dirs = {parent for path in inventory for parent in path.parents}
    remaining = [STATE / 'root', STATE / 'system-images']
    while remaining:
        directory = remaining.pop()
        if lstat_if_present(directory) is None:
            continue
        owned(directory, stat.S_ISDIR)
        for path in directory.iterdir():
            if path in dirs:
                remaining.append(path)
            elif path not in inventory:
                raise StackError('unknown persistent Cubelet data prevents fresh installation recovery')
            else:
                data = inventory[path]
                info = owned(path)
                if info.st_size != data['bytes'] or digest(path) != data['sha256']:
                    raise StackError('partial CVM component differs from the approved fresh release')


def prepare_boot_runtime_state(*, fresh_install=False):
    """Retain Cube databases, but never recover dead shim transports after boot."""
    boot = HOST_BOOT_ID.read_text().strip()
    if not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', boot):
        raise StackError('host boot identity is unavailable')
    metadata = STATE / 'state'
    receipt = metadata / '.brocade-runtime-boot.json'
    runtime = metadata / 'io.containerd.runtime.v2.task'
    previous = read_json(receipt).get('host_boot_id') if receipt.exists() else None
    if receipt.exists() and (
        receipt.is_symlink()
        or not isinstance(previous, str)
        or not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', previous)
    ):
        raise StackError('runtime boot receipt is invalid')
    if runtime.is_symlink() or (
        runtime.exists()
        and (not runtime.is_dir() or runtime.stat().st_uid != os.geteuid() or runtime.stat().st_mode & 0o022)
    ):
        raise StackError('runtime transport directory is not owned and private')
    if previous == boot:
        return
    if previous is None and runtime.exists() and any(runtime.iterdir()):
        raise StackError('existing runtime transports have no managed boot receipt')
    if fresh_install:
        # An interrupted first install cannot authorize discarding transports from
        # a previous attempt even if systemd starts Cubelet after a reboot.
        empty_directory(runtime)
    elif runtime.exists():
        # The exact containerd task directory contains kernel-lifetime sockets
        # and bundles. Cubelet's volume, network and sandbox databases are its
        # siblings. Never traverse a still-mounted live guest filesystem.
        mounts = run(['findmnt', '-rn', '-o', 'TARGET']).splitlines()
        if any(path == str(runtime) or path.startswith(str(runtime) + '/') for path in mounts):
            raise StackError('runtime transports still contain mounted filesystems')
        shutil.rmtree(runtime)
        sync_dir(metadata)
    atomic_json(receipt, {'host_boot_id': boot})


def verify_stack_boot():
    metadata = STATE / 'state'
    boot = HOST_BOOT_ID.read_text().strip()
    if not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', boot) or (
        owned_json(metadata / '.brocade-runtime-boot.json').get('host_boot_id') != boot
    ):
        raise StackError('CVM stack maintenance requires an initialized runtime on the current boot')
    if run(['findmnt', '-n', '-o', 'TARGET', '-T', str(metadata)]) != str(metadata):
        raise StackError('CVM stack maintenance requires the existing Cubelet metadata bind mount')


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
    if lstat_if_present(BASE / STACK_INSTALL_CHECKPOINT) is not None:
        prepare_boot_runtime_state(fresh_install=True)
    else:
        prepare_boot_runtime_state()
    binary = release / 'runtime/Cubelet/bin/cubelet'
    process = subprocess.Popen(
        [
            str(binary),
            '--config',
            str(release / 'generated/cubelet.toml'),
            '--dynamic-conf-path',
            str(release / 'generated/dynamic.yaml'),
        ],
        start_new_session=True,
    )
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        try:
            pid = int(Path('/run/cube-let.pid').read_text().strip())
            if (
                pid > 1
                and Path(f'/proc/{pid}/exe').resolve() == binary
                and Path('/run/cubelet/cubelet.sock').is_socket()
            ):
                return
        except (OSError, ValueError):
            pass
        # Cubelet intentionally kills its launcher parent after entering its mount namespace.
        if process.poll() not in (None, 0, -9):
            raise StackError('Cubelet launcher failed before daemon readiness')
        time.sleep(0.2)
    raise StackError('Cubelet did not publish its actual daemon PID and Unix socket')


def cold_evidence(config, expected, *, read_only=False):
    flag = '--stack-cold-read-only' if read_only else '--stack-cold'
    try:
        result = subprocess.run(
            [str(AGENT_BIN), 'cvm-check', flag, '--config', str(config)],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=60,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise StackError(f'Agent {flag} check did not complete') from None
    if result.returncode:
        raise StackError(f'Agent {flag} check failed (exit {result.returncode})')
    if result.stdout not in (expected.encode(), expected.encode() + b'\n'):
        raise StackError(f'Agent {flag} check did not match the approved preserved roots digest')


def main_pid(unit):
    pid = run(['systemctl', 'show', '--value', '-p', 'MainPID', unit])
    if not re.fullmatch(r'0|[1-9][0-9]{0,9}', pid) or pid == '1':
        raise StackError(f'{unit} did not report a valid MainPID')
    return pid


def running_cubelet(release, expected, pid):
    executable = PROC / pid / 'exe'
    binary = release / CUBELET
    try:
        with executable.open('rb') as stream:
            if not os.path.samestat(os.fstat(stream.fileno()), binary.stat()) or stream_digest(stream) != expected:
                return False
        return main_pid(UNITS[0]) == pid
    except (FileNotFoundError, ProcessLookupError):
        return False


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
            owned_targets = {
                self.release(identity) / 'runtime'
                for identity in (state.get('current'), state.get('pending'))
                if identity
            }
            if not self.toolbox.is_symlink() or self.toolbox.readlink() not in owned_targets:
                raise StackError('existing Cube installation is not managed by this CVM installer')
        for name in UNITS:
            path = self.units / name
            if present(path) and (
                owned(path).st_size > MAX_JSON or not path.read_bytes().startswith(b'# Managed by Brocade CVM\n')
            ):
                raise StackError('existing CVM service unit is not installer-owned')
        if not state.get('current') and not state.get('pending'):
            for directory in (STATE / 'root', STATE / 'state', STATE / 'storage'):
                if directory.exists() and any(
                    item.name != 'lost+found' or not item.is_dir() or any(item.iterdir())
                    for item in directory.iterdir()
                ):
                    raise StackError('existing Cubelet state has no managed CVM installation receipt')

    @property
    def stack_checkpoint(self):
        return self.base / STACK_CHECKPOINT

    @property
    def stack_install_checkpoint(self):
        return self.base / STACK_INSTALL_CHECKPOINT

    def require_no_stack_install_checkpoint(self):
        if lstat_if_present(self.stack_install_checkpoint) is not None:
            raise StackError('interrupted first CVM installation requires stack-install-resume')

    def require_local_operation(self):
        if (
            present(self.base / STACK_MARKER)
            or present(self.stack_checkpoint)
            or present(self.stack_install_checkpoint)
        ):
            raise StackError('remote CVM stack maintenance is in progress')

    def require_stack_marker(self):
        marker = self.base / STACK_MARKER
        if not present(marker):
            raise StackError('CVM stack maintenance marker is required')
        if not owned_json(marker):
            raise StackError('CVM stack maintenance marker has no approved job')

    def stack_state(self, *, interrupted=False):
        state = owned_json(self.state_path)
        if not is_sha256(state.get('current')):
            raise StackError('no managed current CVM release')
        pending = state.get('pending')
        if pending is not None and (not interrupted or not is_sha256(pending)):
            raise StackError('interrupted CVM activation requires recovery before proceeding')
        # Stack commands separately match both activation links against the
        # approved identities, including a partially re-pointed rollback.
        dropin = self.units / 'brocade-agent.service.d/brocade-cvm.conf'
        for path in (self.units / UNITS[0], dropin):
            if owned(path).st_size > MAX_JSON or not path.read_bytes().startswith(b'# Managed by Brocade CVM\n'):
                raise StackError('existing CVM service configuration is not installer-owned')
        environment = set(dropin.read_text().splitlines())
        if (
            not {
                f'Environment=BROCADE_CVM_CONFIG={self.base}/current/generated/cube.json',
                'Environment=BROCADE_CVM_STATE_DIR=/var/lib/brocade/cvm-cube',
            }
            <= environment
        ):
            raise StackError('CVM Agent drop-in does not select the managed configuration and execution state')
        return state

    def stack_links(self, *identities):
        releases = {self.release(identity) for identity in identities}
        for link, targets in (
            (self.base / 'current', releases),
            (self.toolbox, {release / 'runtime' for release in releases}),
        ):
            if not link.is_symlink() or link.lstat().st_uid != os.geteuid() or link.readlink() not in targets:
                raise StackError('CVM runtime links do not match the managed release')

    def stack_artifacts(self, identity, backend, *, staged=False):
        release = self.release(identity)
        owned(release.parent, stat.S_ISDIR)
        manifest = verify_release(release)
        require_storage_support(manifest, backend)
        if any(manifest['files'][name]['mode'] != 0o755 for name in (CUBELET, SHIM)):
            raise StackError('managed Cubelet and Shim artifacts must both be executable')
        artifacts = {
            'bundle_sha256': identity,
            'cubelet_sha256': manifest['files'][CUBELET]['sha256'],
            'shim_sha256': manifest['files'][SHIM]['sha256'],
            'storage_backend': backend,
        }
        receipt = release / BUNDLE_RECEIPT
        if staged or present(receipt):
            if owned_json(receipt) != {'bundle_sha256': identity, 'manifest_sha256': digest(release / 'manifest.json')}:
                raise StackError('retained CVM manifest differs from the verified bundle')
        native = owned_json(release / 'config/network.json')
        self.stack_shim(release, native, artifacts['shim_sha256'])
        return manifest, artifacts

    def stack_shim(self, release, native, expected):
        version_file = release / 'runtime/cube-shim/version'
        if owned(version_file).st_size > 97:
            raise StackError('invalid immutable Shim version')
        version = version_file.read_text().strip()
        if (
            not re.fullmatch(r'[A-Za-z0-9_.-]{1,96}', version)
            or version in ('.', '..')
            or native.get('shim_version') != version
            or native.get('shim_sha256') != expected
            or native.get('state_dir') != '/data/cubelet/network-attachments-v2'
            or 'bridge_sources' in native
        ):
            raise StackError('native network configuration does not pin the exact compatible Shim')
        return version

    def stack_config(self, artifacts):
        release = self.release(artifacts['bundle_sha256'])
        generated = release / 'generated'
        owned(generated, stat.S_ISDIR)
        config = owned_json(generated / 'cube.json')
        backend = artifacts['storage_backend']
        if config.get('virtualization_mode') != 'kvm' or config.get('storage_backend') != backend:
            raise StackError('current CVM stack must use native KVM and the approved storage backend')
        cubelet_config = generated / 'cubelet.toml'
        if owned(cubelet_config).st_size > MAX_JSON:
            raise StackError('managed Cubelet configuration exceeds size limit')
        cubelet = tomllib.loads(cubelet_config.read_text())
        grpc = cubelet.get('grpc')
        if (
            config.get('cubelet_socket') != CUBELET_SOCKET
            or not isinstance(grpc, dict)
            or grpc.get('address') != CUBELET_SOCKET
            or grpc.get('tcp_address')
            or cubelet.get('root') != str(STATE / 'root')
            or cubelet.get('state') != str(STATE / 'state')
        ):
            raise StackError('CVM configuration does not point at the managed Cubelet socket and metadata')
        if present(generated / 'storage.json'):
            owned(generated / 'storage.json')
        verify_storage_binding(release, backend)
        version = self.stack_shim(release, owned_json(generated / 'network.json'), artifacts['shim_sha256'])
        installed = STATE / 'root/component_versions/cube-shim' / version / 'bin' / Path(SHIM).name
        owned(installed)
        if digest(installed) != artifacts['shim_sha256']:
            raise StackError('installed Shim differs from the managed release')
        return generated / 'cube.json'

    def stack_host(self, manifest, identity):
        host_compatibility(manifest, check_virtualization=False)
        host_state = self.base / 'host.json'
        if present(host_state):
            owned_json(host_state)
        host = host_manager(self, self.release(identity))
        if host.state().get('pending') is not None or host.mode() != 'kvm':
            raise StackError('CVM stack maintenance requires native KVM with no host mode transition')
        host.check_live('kvm')

    def stack_selected(self, identity):
        self.stack_links(identity)
        expected = f'ExecStart=/usr/bin/python3 {MANAGER} start-cubelet --release {identity}'
        if expected not in (self.units / UNITS[0]).read_text().splitlines():
            raise StackError('CVM Cubelet unit does not select the managed release')

    def stack_current(self, state, backend, expected=None):
        identity = state['current']
        self.stack_selected(identity)
        manifest, artifacts = self.stack_artifacts(identity, backend)
        if expected is not None and artifacts != expected:
            raise StackError('managed current CVM artifacts differ from the approved source')
        self.stack_host(manifest, identity)
        config = self.stack_config(artifacts)
        pid = main_pid(UNITS[0])
        if (
            pid == '0'
            or run(['systemctl', 'show', '--value', '-p', 'ActiveState', UNITS[0]]) != 'active'
            or not running_cubelet(self.release(identity), artifacts['cubelet_sha256'], pid)
        ):
            raise StackError('running Cubelet does not match the managed current release')
        return manifest, artifacts, config

    def stage_release(self, archive, identity, validate):
        target = self.release(identity)
        if present(target):
            owned(target.parent, stat.S_ISDIR)
            manifest = inspect_archive(archive, identity)
            if verify_release(target) != manifest:
                raise StackError('retained CVM release differs from the verified bundle')
            validate(target, manifest)
            atomic_json(
                target / BUNDLE_RECEIPT,
                {'bundle_sha256': identity, 'manifest_sha256': digest(target / 'manifest.json')},
            )
            return
        durable_mkdir(target.parent)
        owned(target.parent, stat.S_ISDIR)
        with os.scandir(target.parent) as entries:
            if sum(1 for _ in entries) >= MAX_RELEASES:
                raise StackError('CVM retained release limit reached; audit image and instance references first')
        with tempfile.TemporaryDirectory(prefix='.incoming-', dir=target.parent) as temporary:
            stage = Path(temporary) / 'release'
            stage.mkdir(mode=0o700)
            manifest = inspect_archive(archive, identity, stage)
            validate(stage, manifest)
            atomic_json(
                stage / BUNDLE_RECEIPT, {'bundle_sha256': identity, 'manifest_sha256': digest(stage / 'manifest.json')}
            )
            os.rename(stage, target)
            sync_dir(target.parent)

    def stack_target(self, release, manifest, source_manifest, expected):
        host_compatibility(manifest, check_virtualization=False)
        require_storage_support(manifest, expected['storage_backend'])
        if (
            manifest['state_abi'] != source_manifest['state_abi']
            or manifest['files'][CUBELET]['sha256'] != expected['cubelet_sha256']
            or manifest['files'][SHIM]['sha256'] != expected['shim_sha256']
            or any(manifest['files'][name]['mode'] != 0o755 for name in (CUBELET, SHIM))
        ):
            raise StackError('CVM target differs from the approved artifacts or persistent-state ABI')
        if owned_json(release / 'config/cube.json').get('cubelet_socket') != CUBELET_SOCKET:
            raise StackError('CVM target does not use the managed Cubelet socket')
        version = self.stack_shim(release, owned_json(release / 'config/network.json'), expected['shim_sha256'])
        installed = STATE / 'root/component_versions/cube-shim' / version / 'bin' / Path(SHIM).name
        if present(installed):
            owned(installed)
            if digest(installed) != expected['shim_sha256']:
                raise StackError('CVM target Shim version conflicts with a retained installed artifact')

    def stack_stage(self, archive, source, target):
        with self.locked():
            self.require_no_stack_install_checkpoint()
            if present(self.stack_checkpoint):
                raise StackError('interrupted CVM stack maintenance requires stack-recover')
            state = self.stack_state()
            backend = local_storage_backend()
            manifest, _, _ = self.stack_current(state, backend, source)
            self.stage_release(
                archive,
                target['bundle_sha256'],
                lambda release, payload: self.stack_target(release, payload, manifest, target),
            )

    def stack_inspect(self):
        with self.locked():
            self.require_no_stack_install_checkpoint()
            if present(self.stack_checkpoint):
                raise StackError('interrupted CVM stack maintenance requires stack-recover')
            _, artifacts, _ = self.stack_current(self.stack_state(), local_storage_backend())
            return artifacts

    @contextmanager
    def stack_install_locked(self):
        existing_directory(self.base.parent)
        with self.locked():
            owned(self.base / 'install.lock')
            fresh_native_host()
            yield

    def fresh_base(self, identity=None):
        allowed = {'install.lock', 'stack-execute.lock', 'stack-candidate.tar.gz', STACK_MARKER, 'releases'}
        if identity is not None:
            allowed.update(('current', self.state_path.name, STACK_INSTALL_CHECKPOINT))
        for path in self.base.iterdir():
            if path.name not in allowed:
                raise StackError('existing installation or unknown data prevents fresh CVM installation')
            if path.name == STACK_MARKER:
                self.require_stack_marker()
            elif path.name == 'releases':
                owned(path, stat.S_ISDIR)
                for release in path.iterdir():
                    if not is_sha256(release.name):
                        raise StackError('unknown data in the staged CVM release directory')
                    owned(release, stat.S_ISDIR)
                    if release.name != identity:
                        absent(release / 'generated')
                        if next(release.glob('.generated-*'), None) is not None:
                            raise StackError('an incomplete generated CVM configuration is present')
                        manifest = release / 'manifest.json'
                        if owned(manifest).st_size > MAX_JSON or owned_json(release / BUNDLE_RECEIPT) != {
                            'bundle_sha256': release.name,
                            'manifest_sha256': digest(manifest),
                        }:
                            raise StackError('unknown data in the staged CVM release directory')
            elif path.name not in ('current', self.state_path.name, STACK_INSTALL_CHECKPOINT):
                owned(path)

    def fresh_other_state(self, identity=None, *, live=False):
        existing_directory(CVM_CONFIG_DIR)
        existing_directory(EXECUTOR_STATE.parent)
        for path in (*FRESH_FOREIGN_PATHS, CVM_CONFIG_DIR / 'cvm-cube.json', CVM_CONFIG_DIR / 'cvm-host.json'):
            absent(path)
        empty_directory(EXECUTOR_STATE)
        for directory in (self.toolbox.parent, self.units, self.units / f'{AGENT_UNIT}.d'):
            existing_directory(directory)
        # A partial publication can only contain the exact links and service
        # files emitted for this target. No other Cube unit may own the host.
        release = self.release(identity) if identity is not None else None
        for link, target in (
            (self.toolbox, release / 'runtime' if release is not None else None),
            (self.base / 'current', release),
        ):
            absent(link.with_name('.' + link.name + '-next'))
            info = lstat_if_present(link)
            if info is not None and (
                target is None
                or not stat.S_ISLNK(info.st_mode)
                or info.st_uid != os.geteuid()
                or link.readlink() != target
            ):
                raise StackError('CVM runtime links do not match the authorized fresh release')
        unit_files = (
            self.activation_units(identity)
            if release is not None
            else {
                self.units / f'{AGENT_UNIT}.d/brocade-cvm.conf': None,
                self.units / UNITS[0]: None,
            }
        )
        for path, data in unit_files.items():
            if lstat_if_present(path) is not None and (
                data is None or owned(path).st_size != len(data) or path.read_bytes() != data
            ):
                raise StackError('CVM service files do not match the authorized fresh release')
        absent(self.units / f'{UNITS[0]}.d')
        fresh_cube_units((UNITS[0],) if release is not None else ())
        if not live:
            fresh_no_cube_processes()
            empty_directory(CUBELET_RUN)
            absent(CUBELET_PID)
            for directory in RUNTIME_DIRECTORIES:
                existing_directory(directory)
                empty_directory(directory)
        fresh_agent_records(
            {
                'BROCADE_CVM_CONFIG': str(self.base / 'current/generated/cube.json'),
                'BROCADE_CVM_STATE_DIR': str(EXECUTOR_STATE),
            }
            if release is not None and lstat_if_present(self.units / f'{AGENT_UNIT}.d/brocade-cvm.conf')
            else {}
        )

    def fresh_data(self, release=None, manifest=None):
        if lstat_if_present(STATE) is not None:
            owned(STATE, stat.S_ISDIR)
            for entry in STATE.iterdir():
                if entry.name not in ('root', 'state', 'storage') and not (
                    release is not None and entry.name == 'system-images'
                ):
                    raise StackError('unknown persistent Cubelet data prevents fresh installation')
        for target in run(['findmnt', '-rn', '-o', 'TARGET']).splitlines():
            if target.startswith(f'{STATE}/') and target != str(STATE / 'storage'):
                raise StackError('an existing Cubelet metadata or nested storage mount prevents fresh installation')
        empty_directory(STATE / 'state')
        empty_directory(STATE / 'storage', lost_found=True)
        if release is None:
            empty_directory(STATE / 'root')
        else:
            verify_fresh_components(release, manifest)

    def fresh_eligibility(self, *, probe=False):
        self.require_no_stack_install_checkpoint()
        self.fresh_base()
        self.fresh_other_state()
        self.fresh_data()
        return fresh_storage(probe=probe)

    def stack_install_inspect(self):
        with self.stack_install_locked():
            return self.fresh_eligibility(probe=True)['backend']

    def stack_install_target(self, release, manifest, target):
        if manifest['architecture'] != 'x86_64':
            raise StackError('first CVM installation requires an x86_64 bundle')
        self.stack_target(release, manifest, {'state_abi': STATE_ABI}, target)
        # Rendering copies components into host paths computed from these versions.
        fresh_component_inventory(release, manifest)

    def stack_install_stage(self, archive, target):
        with self.stack_install_locked():
            storage = self.fresh_eligibility()
            if storage['backend'] != target['storage_backend']:
                raise StackError('fresh CVM storage differs from the approved backend')

            def validate(release, manifest):
                self.stack_install_target(release, manifest, target)
                for name in ('storage', 'root', 'state'):
                    durable_mkdir(STATE / name)
                self.fresh_data()
                if fresh_storage(probe=True) != storage:
                    raise StackError('fresh CVM storage changed during staging')

            self.stage_release(archive, target['bundle_sha256'], validate)

    def fresh_job(self, target, roots, *, resume):
        if roots != empty_roots_digest(target['storage_backend']):
            raise StackError('fresh CVM installation requires the canonical empty roots digest')
        self.require_stack_marker()
        path = self.base / STACK_MARKER
        if owned(path).st_mode & 0o077:
            raise StackError('CVM fresh installation requires a private Agent job marker')
        marker = owned_json(path)
        offer = marker.get('offer')
        artifact = offer.get('artifact') if isinstance(offer, dict) else None
        payload = artifact.get('payload') if isinstance(artifact, dict) else None
        signed = json.loads(payload) if isinstance(payload, str) else None
        phase = marker.get('phase')
        # Agent verifies the signature and publishes the root-owned claimed marker.
        # Bind Python's eventual mutations to the same signed intent and job.
        expected = {
            'contract': FRESH_INSTALL_CONTRACT,
            'architecture': 'x86_64',
            'target_sha256': target['bundle_sha256'],
            'target_cubelet_sha256': target['cubelet_sha256'],
            'target_shim_sha256': target['shim_sha256'],
            'storage_backend': target['storage_backend'],
        }
        source_fields = ('source_sha256', 'source_cubelet_sha256', 'source_shim_sha256')
        if (
            not isinstance(signed, dict)
            or signed.keys() - expected.keys() - {'target_bytes', *source_fields}
            or any(signed.get(key) is not None for key in source_fields)
            or any(signed.get(key) != value for key, value in expected.items())
            or type(signed.get('target_bytes')) is not int
            or not 1 <= signed['target_bytes'] <= STACK_BUNDLE_MAX_BYTES
            or type(offer.get('id')) is not int
            or offer['id'] <= 0
            or offer.get('status') != 'applying'
            or marker.get('roots_sha256') != roots
            or (phase != 'started' and not (resume and phase == 'recovering'))
        ):
            raise StackError('CVM fresh installation does not match the authorized Agent job')
        return offer['id']

    def fresh_receipt_state(self, identity, job):
        if lstat_if_present(self.state_path) is None:
            return {}
        state = owned_json(self.state_path)
        if state.get('action') == STACK_INSTALL_ACTION and state.get('stack_install_job') == job:
            if set(state) == {'pending', 'action', 'started_at', 'stack_install_job'} and state['pending'] == identity:
                return state
            if (
                set(state) == {'current', 'previous', 'action', 'completed_at', 'stack_install_job'}
                and state['current'] == identity
                and state['previous'] is None
            ):
                return state
        raise StackError('CVM installation state does not match the recorded fresh installation')

    def check_fresh_cubelet_unit(self):
        if run(['systemctl', 'show', '--value', '-p', 'FragmentPath', UNITS[0]]) != str(self.units / UNITS[0]) or run(
            ['systemctl', 'show', '--value', '-p', 'DropInPaths', UNITS[0]]
        ):
            raise StackError('Cubelet has foreign service configuration')

    def fresh_verify_target(self, target, roots, job):
        identity, backend = target['bundle_sha256'], target['storage_backend']
        verify_fresh_config(self.release(identity), job)
        self.fresh_other_state(identity, live=True)
        self.stack_selected(identity)
        for path in self.activation_units(identity):
            owned(path)
        self.check_fresh_cubelet_unit()
        if run(['systemctl', 'show', '--value', '-p', 'Job', UNITS[0]]) not in ('', '0'):
            raise StackError('Cubelet still has a pending service job')
        verify_stack_boot()
        _, _, config = self.stack_current({'current': identity}, backend, target)
        for path in (STATE / 'storage/brocade-roots', STATE / 'storage' / backend / 'objects/volumes'):
            existing_directory(path.parent)
            empty_directory(path)
        cold_evidence(config, roots, read_only=True)

    def stack_install_activate(self, target, roots, *, resume=False):
        with self.stack_install_locked():
            job = self.fresh_job(target, roots, resume=resume)
            identity, backend = target['bundle_sha256'], target['storage_backend']
            release = self.release(identity)
            exists = lstat_if_present(self.stack_install_checkpoint) is not None
            if not resume:
                storage = self.fresh_eligibility(probe=True)
            else:
                self.fresh_base(identity)
                storage = fresh_storage()
            if storage['backend'] != backend:
                raise StackError('fresh CVM storage differs from the approved backend')
            checkpoint = {'job': job, 'target': target, 'roots': roots, 'storage': storage}
            if exists and owned_json(self.stack_install_checkpoint) != checkpoint:
                raise StackError('CVM recovery does not match the recorded fresh installation')
            manifest, artifacts = self.stack_artifacts(identity, backend, staged=True)
            if artifacts != target:
                raise StackError('staged CVM runtime differs from the approved fresh release')
            self.stack_install_target(release, manifest, target)
            state = self.fresh_receipt_state(identity, job) if resume else {}
            if resume and not exists and 'current' not in state:
                raise StackError('no matching interrupted fresh CVM installation to resume')

            unit_exists = lstat_if_present(self.units / UNITS[0]) is not None
            if 'current' in state or (state and unit_exists and main_pid(UNITS[0]) != '0'):
                self.fresh_verify_target(target, roots, job)
                if 'pending' in state:
                    self.commit_change(identity, STACK_INSTALL_ACTION, None, job)
            else:
                if state:
                    if unit_exists and (
                        run(['systemctl', 'show', '--value', '-p', 'ActiveState', UNITS[0]])
                        not in ('inactive', 'failed')
                        or run(['systemctl', 'show', '--value', '-p', 'Job', UNITS[0]]) not in ('', '0')
                    ):
                        raise StackError('Cubelet service is not settled; refusing fresh installation recovery')
                    self.fresh_other_state(identity)
                    self.fresh_data(release, manifest)
                else:
                    self.fresh_other_state()
                    self.fresh_data()
                    absent(release / 'generated')
                if not exists:
                    atomic_json(self.stack_install_checkpoint, checkpoint)
                    exists = True
                self.stop_stack_agent()
                # Recheck after stopping Agent and before any runtime change.
                if self.fresh_job(target, roots, resume=resume) != job or fresh_storage(probe=True) != storage:
                    raise StackError('fresh CVM job or storage changed before starting services')
                self.fresh_other_state(identity if state else None)
                self.fresh_data(release if state else None, manifest)
                if not state:
                    absent(release / 'generated')
                elif lstat_if_present(release / 'generated') is not None:
                    verify_fresh_config(release, job)
                    self.stack_config(target)
                self.change(identity, STACK_INSTALL_ACTION, install_job=job)
                self.fresh_verify_target(target, roots, job)
            if self.fresh_job(target, roots, resume=resume) != job or fresh_storage() != storage:
                raise StackError('fresh CVM job or storage changed before completing installation')
            if exists:
                self.stack_install_checkpoint.unlink()
                sync_dir(self.base)

    def stop_stack_agent(self):
        if run(['systemctl', 'show', '--value', '-p', 'LoadState', AGENT_UNIT]) != 'loaded':
            raise StackError('CVM stack maintenance requires the local Brocade Agent service')
        run(['systemctl', 'stop', AGENT_UNIT], timeout=120)
        if main_pid(AGENT_UNIT) != '0' or run(
            ['systemctl', 'show', '--value', '-p', 'ActiveState', AGENT_UNIT]
        ) not in ('inactive', 'failed'):
            raise StackError('Brocade Agent did not stop before the exclusive cold check')

    def finish_stack_checkpoint(self):
        self.require_stack_marker()
        self.stack_checkpoint.unlink()
        sync_dir(self.base)

    def stack_activate(self, source, target, roots):
        with self.locked():
            self.require_no_stack_install_checkpoint()
            self.require_stack_marker()
            if present(self.stack_checkpoint):
                raise StackError('interrupted CVM stack maintenance requires stack-recover')
            state = self.stack_state()
            backend = local_storage_backend()
            old, _, config = self.stack_current(state, backend, source)
            new, _ = self.stack_artifacts(target['bundle_sha256'], backend, staged=True)
            self.stack_target(self.release(target['bundle_sha256']), new, old, target)
            verify_stack_boot()
            previous = state.get('previous')
            if previous is not None and not is_sha256(previous):
                raise StackError('CVM installation has an invalid retained rollback identity')
            checkpoint = {'source': source, 'target': target, 'previous': previous, 'roots': roots, 'phase': 'stopping'}
            atomic_json(self.stack_checkpoint, checkpoint)
            try:
                self.stop_stack_agent()
                cold_evidence(config, roots)
                self.require_stack_marker()
            except (StackError, OSError, subprocess.SubprocessError):
                # No Cubelet changes have started. Keep the fence/checkpoint for
                # explicit verification, but restore ordinary Agent service.
                self.require_stack_marker()
                run(['systemctl', 'start', AGENT_UNIT], timeout=90)
                raise
            atomic_json(self.stack_checkpoint, {**checkpoint, 'phase': 'switching'})
            self.change(target['bundle_sha256'], 'stack-activate')
            _, _, config = self.stack_current(self.stack_state(), backend, target)
            self.require_stack_marker()
            cold_evidence(config, roots, read_only=True)
            self.finish_stack_checkpoint()

    def recovery_checkpoint(self, identity, roots, backend):
        value = owned_json(self.stack_checkpoint)
        if (
            value.get('phase') not in ('stopping', 'switching', 'recovering')
            or value.get('roots') != roots
            or (value.get('previous') is not None and not is_sha256(value['previous']))
            or any(
                not isinstance(item, dict)
                or set(item) != {'bundle_sha256', 'cubelet_sha256', 'shim_sha256', 'storage_backend'}
                or item['storage_backend'] != backend
                or any(not is_sha256(item[key]) for key in ('bundle_sha256', 'cubelet_sha256', 'shim_sha256'))
                for item in (value.get('source'), value.get('target'))
            )
            or value['source']['bundle_sha256'] != identity
            or value['target']['bundle_sha256'] == identity
        ):
            raise StackError('CVM recovery does not match the recorded cold stack upgrade')
        return value

    def stack_recover(self, identity, roots):
        with self.locked():
            self.require_no_stack_install_checkpoint()
            self.require_stack_marker()
            state = self.stack_state(interrupted=True)
            backend = local_storage_backend()
            if not present(self.stack_checkpoint):
                if state['current'] != identity or state.get('pending') is not None:
                    raise StackError('no matching interrupted remote CVM stack upgrade to recover')
                _, _, config = self.stack_current(state, backend)
                run(['systemctl', 'start', AGENT_UNIT], timeout=90)
                cold_evidence(config, roots, read_only=True)
                self.require_stack_marker()
                return

            checkpoint = self.recovery_checkpoint(identity, roots, backend)
            candidate = checkpoint['target']['bundle_sha256']
            current, pending, phase = state['current'], state.get('pending'), checkpoint['phase']
            if not (
                (current == identity and pending is None)
                or (
                    phase != 'stopping'
                    and current == identity
                    and pending == candidate
                    and state.get('action') == 'stack-activate'
                )
                or (
                    phase != 'stopping'
                    and current == candidate
                    and pending is None
                    and state.get('action') == 'stack-activate'
                    and state.get('previous') == identity
                )
                or (
                    phase == 'recovering'
                    and pending == identity
                    and state.get('action') == 'stack-recover'
                    and current in (identity, candidate)
                )
            ):
                raise StackError('CVM installation state does not match the interrupted remote upgrade')
            self.stack_links(identity, candidate)
            manifest, source = self.stack_artifacts(identity, backend)
            if source != checkpoint['source']:
                raise StackError('retained CVM source no longer matches the approved artifacts')
            self.stack_host(manifest, identity)
            config = self.stack_config(source)
            pid = main_pid(UNITS[0])
            active = run(['systemctl', 'show', '--value', '-p', 'ActiveState', UNITS[0]])
            if run(['systemctl', 'show', '--value', '-p', 'Job', UNITS[0]]) not in ('', '0'):
                raise StackError('a Cubelet service job is still pending; no recovery switch was attempted')
            if pid == '0':
                if active not in ('failed', 'inactive') or phase == 'stopping':
                    raise StackError('CVM Cubelet state is not settled; no recovery switch was attempted')
                selected = None
            elif active == 'active' and running_cubelet(self.release(identity), source['cubelet_sha256'], pid):
                selected = identity
            elif (
                phase != 'stopping'
                and active == 'active'
                and running_cubelet(self.release(candidate), checkpoint['target']['cubelet_sha256'], pid)
            ):
                selected = candidate
            else:
                raise StackError(
                    'CVM Cubelet is not running a known managed artifact; no recovery switch was attempted'
                )

            # If the source is already selected and serving, recovery is proof
            # only, including a retry after a lost recovery response.
            source_selected = selected == identity and all(
                link.readlink() == path
                for link, path in (
                    (self.base / 'current', self.release(identity)),
                    (self.toolbox, self.release(identity) / 'runtime'),
                )
            )
            if source_selected:
                self.stack_selected(identity)
                run(['systemctl', 'start', AGENT_UNIT], timeout=90)
            else:
                if phase != 'switching':
                    raise StackError('CVM recovery outcome is unknown; refusing to repeat a service switch')
                verify_stack_boot()
                self.stop_stack_agent()
                if selected:
                    evidence_config = config
                    if selected == candidate:
                        _, live = self.stack_artifacts(candidate, backend, staged=True)
                        if live != checkpoint['target']:
                            raise StackError('running CVM target no longer matches the approved artifacts')
                        evidence_config = self.stack_config(live)
                    cold_evidence(evidence_config, roots)
                # With a settled, stopped Cubelet the last exclusive cold proof
                # (recorded before the switch) and the continuous fence are the
                # only available evidence until the source can answer List again.
                self.require_stack_marker()
                atomic_json(self.stack_checkpoint, {**checkpoint, 'phase': 'recovering'})
                atomic_json(self.state_path, {**state, 'pending': identity, 'action': 'stack-recover'})
                self.activate(identity, restore=True)
                self.stack_current({'current': identity}, backend, source)

            self.require_stack_marker()
            cold_evidence(config, roots, read_only=True)
            if pending is not None or current != identity:
                atomic_json(
                    self.state_path,
                    {
                        'current': identity,
                        'previous': checkpoint.get('previous'),
                        'action': 'stack-recover',
                        'recovered_at': timestamp(),
                    },
                )
            self.finish_stack_checkpoint()

    def activation_units(self, identity):
        release = self.release(identity)
        dropin = self.units / 'brocade-agent.service.d/brocade-cvm.conf'
        common = '# Managed by Brocade CVM\n'
        cubelet = (
            common
            + f"""[Unit]
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
"""
        )
        settings = (
            common
            + f"""[Unit]
After=brocade-cvm-cubelet.service
Wants=brocade-cvm-cubelet.service
[Service]
Environment=BROCADE_CVM_CONFIG={self.base}/current/generated/cube.json
Environment=BROCADE_CVM_STATE_DIR=/var/lib/brocade/cvm-cube
"""
        )
        return {dropin: settings.encode(), self.units / UNITS[0]: cubelet.encode()}

    def activate(self, identity, *, restore=False, install_job=None):
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
        host = host_manager(self, release)
        host.check_live(host.mode())
        if not restore:
            prepare_runtime_directories()
            state = self.state()
            previous = self.release(state['current']) if state.get('current') and state['current'] != identity else None
            if previous is None and (release / 'generated/cube.json').exists():
                previous = release
            if install_job is not None:
                render_configuration(release, mode='kvm', install_job=install_job)
            else:
                render_configuration(release, previous, mode=host.mode())
        report_stage('activate services')
        point(self.toolbox, release / 'runtime')
        point(self.base / 'current', release)
        self.units.mkdir(parents=True, exist_ok=True)
        dropin.parent.mkdir(parents=True, exist_ok=True)
        for path, contents in self.activation_units(identity).items():
            atomic(path, contents, 0o644)
        run(['systemctl', 'daemon-reload'])
        if install_job is not None:
            self.check_fresh_cubelet_unit()
        run(['systemctl', 'enable', *UNITS])
        run(['systemctl', 'restart', UNITS[0]], timeout=115)
        pid = run(['systemctl', 'show', '--value', '-p', 'MainPID', UNITS[0]])
        binary = release / 'runtime/Cubelet/bin/cubelet'
        if not pid.isdigit() or int(pid) < 2 or digest(Path('/proc') / pid / 'exe') != digest(binary):
            raise StackError('running Cubelet does not match the activated artifact')
        if run(['nsenter', '-t', pid, '-m', 'findmnt', '-n', '-o', 'FSTYPE', '-T', str(STATE / 'state')]) in (
            'tmpfs',
            'ramfs',
        ):
            raise StackError('running Cubelet metadata is not persistent')
        # Fresh enrollment writes and starts the Agent unit after this optional
        # stack step. Existing installations reload the new configuration now.
        if run(['systemctl', 'show', '--value', '-p', 'LoadState', 'brocade-agent.service']) == 'loaded':
            run(['systemctl', 'restart', 'brocade-agent.service'])
        report_stage('verify actual capabilities')
        ready(config=release / 'generated/cube.json')

    def commit_change(self, identity, action, previous, install_job=None):
        state = {'current': identity, 'previous': previous, 'action': action, 'completed_at': timestamp()}
        if install_job is not None:
            state['stack_install_job'] = install_job
        atomic_json(self.state_path, state)

    def change(self, identity, action, force=False, *, install_job=None):
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
        if install_job is not None:
            pending['stack_install_job'] = install_job
        atomic_json(self.state_path, pending)
        try:
            if install_job is not None:
                # A resumed first install has already verified the generated configuration.
                self.activate(
                    identity,
                    install_job=install_job,
                    restore=lstat_if_present(self.release(identity) / 'generated') is not None,
                )
            else:
                self.activate(identity)
        except BaseException:
            if action in ('stack-activate', STACK_INSTALL_ACTION):
                # An unknown outcome is not permission to switch again. The
                # fenced recovery path first verifies ownership and cold evidence.
                raise
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
        self.commit_change(identity, action, retained_previous, install_job)

    def install(self, archive, expected, mode=None):
        with self.locked():
            self.require_local_operation()
            state = self.state()
            self.check_ownership(state)

            def validate(_, manifest):
                report_stage('host preflight')
                preflight(manifest, check_virtualization=False)

            report_stage('verify and stage immutable release')
            self.stage_release(archive, expected, validate)
            self.require_local_operation()
            host = host_manager(self, self.release(expected))
            if host.prepare(mode):
                return True
            persist_boot_modules()
            self.change(expected, 'install', force=bool(host.state().get('pending')))
            host.commit()
            return False

    def set_mode(self, mode, identity=None):
        with self.locked():
            self.require_local_operation()
            identity = identity or self.state().get('current')
            if not identity:
                raise StackError('install a CVM runtime before selecting its mode')
            release = self.release(identity)
            preflight(verify_release(release), check_virtualization=False)
            host = host_manager(self, release)
            self.require_local_operation()
            pending = host.state().get('pending')
            if (
                pending
                and HOST_BOOT_ID.read_text().strip() != pending['boot_id']
                and platform.release() != pending['kernel']
            ):
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
            self.require_local_operation()
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
    manifest = validate_manifest(
        {
            'contract': CONTRACT,
            'state_abi': STATE_ABI,
            'architecture': architecture,
            'source_sha256': source_sha256,
            'min_glibc': min_glibc,
            'files': files,
            'storage_backends': ['xfs'] if storage_backends is None else storage_backends,
            'provenance': provenance,
        }
    )
    if pvm is not None:
        manifest['pvm'] = pvm
    output.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with (
        os.fdopen(fd, 'wb') as raw,
        gzip.GzipFile(fileobj=raw, mode='wb', mtime=0, filename='') as compressed,
        tarfile.open(fileobj=compressed, mode='w') as archive,
    ):
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


def sha256_arg(value):
    if not is_sha256(value):
        raise argparse.ArgumentTypeError('expected a lowercase SHA-256 digest')
    return value


def stack_intent(args, role):
    return {
        'bundle_sha256': getattr(args, role),
        'cubelet_sha256': getattr(args, f'{role}_cubelet'),
        'shim_sha256': getattr(args, f'{role}_shim'),
        'storage_backend': args.storage,
    }


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
    rollback = sub.add_parser('rollback')
    rollback.add_argument(
        '--release', help='explicit retained release SHA-256; omitted retries retain the last rollback target'
    )
    sub.add_parser('recover')
    start = sub.add_parser('start-cubelet')
    start.add_argument('--release', required=True)
    for command in ('stack-stage', 'stack-activate'):
        p = sub.add_parser(command)
        p.add_argument('--storage', choices=('xfs', 'plain'), required=True)
        for flag in ('source', 'target', 'source-cubelet', 'target-cubelet', 'source-shim', 'target-shim'):
            p.add_argument(f'--{flag}', type=sha256_arg, required=True)
        if command == 'stack-stage':
            p.add_argument('--bundle', type=Path, required=True)
        else:
            p.add_argument('--expected-roots-sha256', type=sha256_arg, required=True)
    sub.add_parser('stack-inspect')
    sub.add_parser('stack-install-inspect')
    for command in ('stack-install-stage', STACK_INSTALL_ACTION, 'stack-install-resume'):
        p = sub.add_parser(command)
        p.add_argument('--storage', choices=('xfs', 'plain'), required=True)
        for flag in ('target', 'target-cubelet', 'target-shim'):
            p.add_argument(f'--{flag}', type=sha256_arg, required=True)
        if command == 'stack-install-stage':
            p.add_argument('--bundle', type=Path, required=True)
        else:
            p.add_argument('--expected-roots-sha256', type=sha256_arg, required=True)
    recover = sub.add_parser('stack-recover')
    recover.add_argument('--source', type=sha256_arg, required=True)
    recover.add_argument('--expected-roots-sha256', type=sha256_arg, required=True)
    args = parser.parse_args()
    if args.command in ('stack-stage', 'stack-activate') and args.source == args.target:
        parser.error('source and target CVM bundles must differ')
    if args.command in (
        'install',
        'check',
        'rollback',
        'recover',
        'pack',
        'mode',
        'resume',
        'stack-stage',
        'stack-activate',
        'stack-recover',
        'stack-install-stage',
        STACK_INSTALL_ACTION,
        'stack-install-resume',
    ):
        REPORTER = ProgressReporter(args.command)
    install = Installation()
    if args.command == 'pack':
        pack(args.node_tree, args.output, args.architecture, args.source_sha256, args.min_glibc)
    elif args.command == 'check':
        with install.locked():
            install.require_local_operation()
            preflight(inspect_archive(args.bundle, args.sha256))
        print('CVM bundle and host preflight passed')
    elif args.command == 'stack-stage':
        install.stack_stage(args.bundle, stack_intent(args, 'source'), stack_intent(args, 'target'))
    elif args.command == 'stack-activate':
        install.stack_activate(stack_intent(args, 'source'), stack_intent(args, 'target'), args.expected_roots_sha256)
    elif args.command == 'stack-inspect':
        print(json.dumps(install.stack_inspect(), sort_keys=True))
    elif args.command == 'stack-recover':
        install.stack_recover(args.source, args.expected_roots_sha256)
    elif args.command == 'stack-install-inspect':
        print(json.dumps(install.stack_install_inspect()))
    elif args.command == 'stack-install-stage':
        install.stack_install_stage(args.bundle, stack_intent(args, 'target'))
    elif args.command in (STACK_INSTALL_ACTION, 'stack-install-resume'):
        install.stack_install_activate(
            stack_intent(args, 'target'), args.expected_roots_sha256, resume=args.command == 'stack-install-resume'
        )
    elif args.command == 'install':
        if install.install(args.bundle, args.sha256, args.mode):
            pending_reboot()
    elif args.command in ('mode', 'resume'):
        if install.set_mode(
            args.selection if args.command == 'mode' else None, args.release if args.command == 'resume' else None
        ):
            pending_reboot()
    elif args.command == 'cancel-mode':
        with install.locked():
            install.require_local_operation()
            state = read_json(install.base / 'host.json')
            if not state.get('pending'):
                raise StackError('there is no pending virtualization transition')
            host = host_manager(install, install.release(state['pending']['release']))
            host.cancel()
    elif args.command == 'rollback':
        with install.locked():
            install.require_local_operation()
            state = install.state()
            previous = args.release or state.get('current' if state.get('action') == 'rollback' else 'previous')
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
            install.require_local_operation()
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
    print(
        'CVM virtualization is prepared; reboot the host manually. Installation will resume automatically after boot.',
        file=sys.stderr,
    )
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
