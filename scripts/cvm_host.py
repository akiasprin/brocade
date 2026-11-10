"""Pinned host virtualization transitions, loaded only from a verified CVM bundle.

Native KVM is preferred. PVM needs a paired host/Guest kernel and one host reboot;
the transition is durable and never reboots the host itself. A one-shot GRUB entry
keeps the last working kernel available if the candidate cannot boot.
"""
import errno
import fcntl
import os
from pathlib import Path
import platform
import re
import shlex
import stat


MODES = ('auto', 'kvm', 'pvm')
HEADER = '# Managed by Brocade CVM\n'
SCRIPT_HEADER = '#!/bin/sh\n' + HEADER
GRUB_SCRIPT = Path('/etc/grub.d/42_brocade_cvm')
GRUB_DEFAULT = Path('/etc/default/grub.d/99-brocade-cvm.cfg')
GRUB_CONFIG = Path('/boot/grub/grub.cfg')
PVM_MODULE = Path('/sys/module/kvm_pvm')
CPUINFO = Path('/proc/cpuinfo')
CMDLINE = Path('/proc/cmdline')


def probe():
    """A visible device alone is insufficient: prove that creating a VM works."""
    try:
        with open('/dev/kvm', 'r+b', buffering=0) as device:
            if fcntl.ioctl(device.fileno(), 0xAE00, 0) != 12:
                raise ValueError('unsupported KVM API')
            vm = fcntl.ioctl(device.fileno(), 0xAE01, 0)
            os.close(vm)
    except OSError as error:
        if error.errno in (errno.ENOENT, errno.ENODEV, errno.ENXIO, errno.EOPNOTSUPP):
            return None
        # Permission, file type and I/O errors must not silently trigger a kernel change.
        raise
    return 'pvm' if PVM_MODULE.is_dir() else 'kvm'


def boot_entry(source, kernel, identifier, arguments=()):
    """Clone the distribution's own entry, including its root and initrd setup.

    GRUB's generated Linux entries have their opening/closing braces on the menu
    line / a standalone line. Nested shell blocks use `if/fi`, not brace blocks.
    Reject an unfamiliar shape instead of guessing a disk or a Debian menu title.
    """
    entries = re.findall(r'^[ \t]*menuentry [^\n]+\{\n.*?^[ \t]*\}[ \t]*$', source, re.M | re.S)
    matches = []
    signatures = []
    for entry in entries:
        if 'brocade-cvm-' in entry.splitlines()[0] or 'recovery' in entry.splitlines()[0]:
            continue
        lines = entry.splitlines()
        linux = [i for i, line in enumerate(lines) if re.match(r'\s*linux(?:efi)?\s', line)]
        if len(linux) != 1:
            continue
        tokens = shlex.split(lines[linux[0]])
        if len(tokens) < 2 or Path(tokens[1]).name != 'vmlinuz-' + kernel:
            continue
        if not any(re.match(r'\s*initrd(?:efi)?\s', line) for line in lines):
            continue
        signatures.append(tuple(line.strip() for line in lines if re.match(r'\s*(?:linux(?:efi)?|initrd(?:efi)?|search)\s', line)))
        lines[0] = f"menuentry 'Brocade CVM {kernel}' --id '{identifier}' {{"
        lines[linux[0]] += ''.join(' ' + arg for arg in arguments)
        matches.append('\n'.join(lines) + '\n')
    if not matches or any(signature != signatures[0] for signature in signatures):
        raise ValueError('cannot identify an unambiguous generated Linux GRUB entry')
    return matches[0]


def validate(value, files):
    if value is None:
        return
    if not isinstance(value, dict) or set(value) != {'host_kernel_release', 'host_package', 'guest_kernel', 'boot_args'}:
        raise ValueError('invalid PVM kernel inventory')
    release = value['host_kernel_release']
    if not isinstance(release, str) or not re.fullmatch(r'[A-Za-z0-9_.+-]{1,160}', release) or 'pvm.host' not in release:
        raise ValueError('invalid pinned PVM host kernel release')
    if not isinstance(value['host_package'], str) or value['host_package'] not in files or not value['host_package'].startswith('runtime/pvm/') or not value['host_package'].endswith('.deb'):
        raise ValueError('PVM requires an inventoried Debian host kernel package')
    if value['guest_kernel'] != 'runtime/cube-kernel-pvm/vmlinux' or value['guest_kernel'] not in files or 'runtime/cube-kernel-pvm/version' not in files:
        raise ValueError('PVM Guest kernel inventory is incomplete')
    args = value['boot_args']
    if not isinstance(args, list) or any(not isinstance(arg, str) or arg not in ('nopti', 'pti=off', 'no5lvl', 'rt_group_sched=0') for arg in args) or len(args) != len(set(args)):
        raise ValueError('unsupported PVM kernel boot arguments')
    for name in ('runtime/bin/cvm-host.py', 'runtime/bin/cvm-stack.py', 'config/cube-pvm.json', 'config/system-pvm.json'):
        if name not in files:
            raise ValueError('PVM transition or Guest configuration is missing')


class Host:
    def __init__(self, stack, installation, release):
        self.s, self.installation, self.release = stack, installation, release
        self.path = installation.base / 'host.json'
        self.manifest = stack.verify_release(release)
        self.pvm = self.manifest.get('pvm')

    def state(self):
        state = self.s.read_json(self.path) if self.path.exists() else {}
        if not isinstance(state, dict) or state.get('desired', 'auto') not in MODES or state.get('effective', 'kvm') not in ('kvm', 'pvm'):
            raise self.s.StackError('invalid CVM virtualization state')
        pending = state.get('pending')
        if pending is not None and (not isinstance(pending, dict)
                or pending.get('mode') not in ('kvm', 'pvm')
                or pending.get('phase') not in ('preparing', 'awaiting_reboot', 'activating', 'failed')
                or not isinstance(pending.get('release'), str) or not re.fullmatch(r'[0-9a-f]{64}', pending['release'])
                or not isinstance(pending.get('kernel'), str) or not re.fullmatch(r'[A-Za-z0-9_.+-]{1,160}', pending['kernel'])
                or not isinstance(pending.get('boot_id'), str) or not 1 <= len(pending['boot_id']) <= 128):
            raise self.s.StackError('invalid pending CVM virtualization transition')
        return state

    def save(self, value):
        self.s.atomic_json(self.path, value)

    def mode(self):
        state = self.state()
        pending = state.get('pending')
        return pending['mode'] if pending else state.get('effective', 'kvm')

    def modules(self):
        # Never unload a module: a running VM may still own it.
        for module in ('kvm_pvm',) if 'pvm.host' in platform.release() else ('kvm_intel', 'kvm_amd'):
            result = self.s.subprocess.run(['modprobe', module], capture_output=True, timeout=15)
            if result.returncode == 0 and probe():
                break

    def check_live(self, mode):
        actual = probe()
        if actual != mode:
            raise self.s.StackError(f'CVM requires {mode}; the running kernel does not provide it')
        if mode == 'pvm':
            if not self.pvm or platform.release() != self.pvm['host_kernel_release']:
                raise self.s.StackError('running PVM host kernel does not match the pinned kernel pair')
            cmdline = CMDLINE.read_text().split()
            if any(arg not in cmdline for arg in self.pvm['boot_args']):
                raise self.s.StackError('running PVM host is missing required boot arguments')

    def owned(self, path):
        if path.exists() or path.is_symlink():
            info = path.lstat()
            prefix = SCRIPT_HEADER if path == GRUB_SCRIPT else HEADER
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022 or not path.read_text().startswith(prefix):
                raise self.s.StackError('existing CVM boot configuration is not installer-owned')

    def boot_tools(self):
        if self.s.run([str(self.s.AGENT_BIN), 'cvm-check', '--maintenance-contract']) != 'brocade-cvm-maintenance-v1':
            raise self.s.StackError('upgrade Agent to the CVM maintenance contract before preparing a host kernel')
        if not all(self.s.shutil.which(tool) for tool in ('update-grub', 'grub-reboot', 'grub-set-default', 'grub-editenv', 'dpkg', 'dpkg-deb')):
            raise self.s.StackError('automatic PVM host preparation requires Debian/Ubuntu with GRUB and dpkg')
        if not GRUB_CONFIG.is_file() or not GRUB_DEFAULT.parent.is_dir():
            raise self.s.StackError('managed GRUB configuration is unavailable; prepare the PVM kernel manually')
        for path in (GRUB_SCRIPT, GRUB_DEFAULT, self.installation.units / 'brocade-cvm-resume.service'):
            self.owned(path)
        # Fail before mutation if the GRUB environment cannot even be read.
        self.s.run(['grub-editenv', '-', 'list'])

    def package(self):
        package = self.release / self.pvm['host_package']
        package_name = self.s.run(['dpkg-deb', '-f', str(package), 'Package'])
        arch = self.s.run(['dpkg-deb', '-f', str(package), 'Architecture'])
        if package_name != 'linux-image-' + self.pvm['host_kernel_release'] or arch != 'amd64':
            raise self.s.StackError('PVM package identity does not match its pinned kernel release')
        return package

    def write_boot(self, entries):
        # A quoted heredoc preserves GRUB variables instead of expanding them as sh.
        self.s.atomic(GRUB_SCRIPT, (SCRIPT_HEADER + "cat <<'BROCADE_CVM_GRUB'\n" + entries + 'BROCADE_CVM_GRUB\n').encode(), 0o755)
        self.s.atomic(GRUB_DEFAULT, (HEADER + 'GRUB_DEFAULT=saved\nGRUB_SAVEDEFAULT=false\n').encode(), 0o644)
        self.s.run(['update-grub'])

    def resume_unit(self):
        unit = HEADER + f'''[Unit]
Description=Complete a prepared CVM virtualization transition
After=network-online.target
Wants=network-online.target
[Service]
Type=oneshot
ExecStart=/usr/bin/python3 {self.release}/runtime/bin/cvm-stack.py resume --release {self.release.name}
TimeoutStartSec=300
[Install]
WantedBy=multi-user.target
'''
        self.s.atomic(self.installation.units / 'brocade-cvm-resume.service', unit.encode(), 0o644)
        self.s.run(['systemctl', 'daemon-reload'])
        self.s.run(['systemctl', 'enable', 'brocade-cvm-resume.service'])

    def prepare(self, requested=None):
        state = self.state()
        desired = requested or state.get('desired', 'auto')
        if desired not in MODES:
            raise self.s.StackError('virtualization mode must be auto, kvm or pvm')
        if state.get('pending'):
            pending = state['pending']
            if pending['release'] != self.release.name or desired != state['desired']:
                raise self.s.StackError('a virtualization transition is pending; resume or cancel it before switching')
            if self.s.HOST_BOOT_ID.read_text().strip() == pending['boot_id']:
                if pending['phase'] == 'activating':
                    self.check_live(pending['mode'])
                    return False
                if pending['phase'] != 'awaiting_reboot':
                    self.stage_boot()
                return True
            self.modules()
            self.check_live(pending['mode'])
            return False
        self.modules()
        actual = probe()
        mode = actual if desired == 'auto' and actual else ('pvm' if desired == 'auto' else desired)
        if mode == 'pvm' and not self.pvm:
            raise self.s.StackError('KVM is unavailable or PVM was selected, but this bundle has no pinned PVM host/Guest kernels; publish a PVM-capable bundle')
        if actual == mode and (mode != 'pvm' or platform.release() == self.pvm['host_kernel_release']):
            self.check_live(mode)
            if self.installation.state().get('current') and state.get('effective', 'kvm') != mode:
                pending = {'mode': mode, 'kernel': platform.release(), 'release': self.release.name,
                           'boot_id': self.s.HOST_BOOT_ID.read_text().strip(), 'phase': 'activating'}
                previous = {key: value for key, value in state.items() if key != 'previous'}
                self.save({**state, 'desired': desired, 'pending': pending, 'previous': previous})
                self.quiesce()
                self.backup_configuration()
            else:
                self.save({**state, 'desired': desired, 'effective': mode})
            return False
        if mode == 'kvm' and not re.search(r'\b(?:vmx|svm)\b', CPUINFO.read_text()):
            raise self.s.StackError('native KVM was selected but this CPU does not expose VMX/SVM; retain PVM or enable nested virtualization')
        self.boot_tools()
        if mode == 'pvm':
            self.package()
        native_kernel = state.get('native_kernel') or (platform.release() if actual != 'pvm' else None)
        if not native_kernel:
            raise self.s.StackError('the original native kernel is unknown; select a verified native kernel manually before switching')
        native_id = 'brocade-cvm-native'
        if not re.fullmatch(r'[A-Za-z0-9_.+-]{1,160}', native_kernel):
            raise self.s.StackError('invalid original kernel identity')
        native_entry = state.get('native_entry') or boot_entry(GRUB_CONFIG.read_text(), native_kernel, native_id)
        target_kernel = self.pvm['host_kernel_release'] if mode == 'pvm' else native_kernel
        pending = {'mode': mode, 'kernel': target_kernel, 'release': self.release.name,
                   'boot_id': self.s.HOST_BOOT_ID.read_text().strip(), 'phase': 'preparing'}
        previous = {key: value for key, value in state.items() if key != 'previous'}
        self.save({**state, 'desired': desired, 'native_kernel': native_kernel, 'native_entry': native_entry,
                   'pending': pending, 'previous': previous, 'started_at': self.s.timestamp()})
        self.stage_boot()
        return True

    def backup_configuration(self):
        current = self.installation.state().get('current')
        if not current:
            return
        backup = self.installation.base / 'host-backup'
        if backup.exists():
            receipt = self.check_backup(backup)
            if receipt['release'] != current:
                raise self.s.StackError('a previous host transition backup needs recovery')
            return
        source = self.installation.release(current) / 'generated'
        if not source.is_dir() or source.is_symlink():
            raise self.s.StackError('the active CVM configuration cannot be backed up')
        paths = list(source.rglob('*'))
        if len(paths) > 128:
            raise self.s.StackError('generated CVM configuration exceeds the transition backup limit')
        with self.s.tempfile.TemporaryDirectory(prefix='.host-backup-', dir=self.installation.base) as temporary:
            stage = Path(temporary) / 'snapshot'
            stage.mkdir(mode=0o700)
            files = {}
            for path in paths:
                if path.is_symlink() or not (path.is_file() or path.is_dir()):
                    raise self.s.StackError('unsafe generated CVM configuration in transition backup')
                if not path.is_file():
                    continue
                name = path.relative_to(source).as_posix()
                if not self.s.safe_name(name) or path.stat().st_size > self.s.MAX_JSON:
                    raise self.s.StackError('invalid generated CVM configuration in transition backup')
                self.s.atomic(stage / name, path.read_bytes())
                files[name] = self.s.digest(stage / name)
            self.s.atomic_json(stage / 'receipt.json', {'release': current, 'files': files})
            os.rename(stage, backup)
            self.s.sync_dir(backup.parent)

    def restore_configuration(self):
        backup = self.installation.base / 'host-backup'
        if not backup.exists():
            return
        receipt = self.check_backup(backup)
        target = self.installation.release(receipt['release']) / 'generated'
        for name in receipt['files']:
            self.s.atomic(target / name, (backup / name).read_bytes())

    def discard_backup(self):
        backup = self.installation.base / 'host-backup'
        if backup.exists():
            # Only our bounded snapshot with a valid receipt may be removed.
            self.check_backup(backup)
            self.s.shutil.rmtree(backup)
            self.s.sync_dir(backup.parent)

    def check_backup(self, backup):
        if backup.is_symlink() or not backup.is_dir() or backup.stat().st_uid != os.geteuid() or backup.stat().st_mode & 0o077:
            raise self.s.StackError('CVM transition backup is not owned and private')
        receipt = self.s.read_json(backup / 'receipt.json')
        if not isinstance(receipt, dict) or not isinstance(receipt.get('release'), str) or not re.fullmatch(r'[0-9a-f]{64}', receipt['release']) or not isinstance(receipt.get('files'), dict) or not 1 <= len(receipt['files']) <= 128:
            raise self.s.StackError('invalid CVM transition backup')
        for name, digest in receipt['files'].items():
            if not self.s.safe_name(name) or (backup / name).is_symlink() or self.s.digest(backup / name) != digest:
                raise self.s.StackError('invalid CVM transition backup digest')
        return receipt

    def stage_boot(self):
        state = self.state()
        pending = state['pending']
        mode, target_kernel = pending['mode'], pending['kernel']
        native_entry = state['native_entry']
        self.boot_tools()
        if mode == 'pvm':
            self.package()
        try:
            # Persist the maintenance marker before stopping the Agent. A restarted
            # Agent continues its other domains but cannot allocate a new CVM.
            self.quiesce()
            self.backup_configuration()
            self.resume_unit()
            current = self.state()
            current['pending']['boot_changed'] = True
            self.save(current)
            self.write_boot(native_entry + state.get('pvm_entry', ''))
            self.s.run(['grub-set-default', state.get('boot_entry', 'brocade-cvm-native')])
            if mode == 'pvm':
                package = self.package()
                self.s.run(['dpkg', '--install', str(package)], timeout=600)
                target = boot_entry(GRUB_CONFIG.read_text(), target_kernel, 'brocade-cvm-pvm', self.pvm['boot_args'])
                state['pvm_entry'] = target
            else:
                target = state.get('pvm_entry', '')
            entries = native_entry + target
            self.write_boot(entries)
            self.s.run(['grub-reboot', 'brocade-cvm-' + ('pvm' if mode == 'pvm' else 'native')])
            current = self.state()
            current['pending']['phase'] = 'awaiting_reboot'
            current['pending']['boot_entry'] = 'brocade-cvm-' + ('pvm' if mode == 'pvm' else 'native')
            if mode == 'pvm':
                current['pvm_entry'] = target
            self.save(current)
        except BaseException:
            current = self.state()
            if current['pending'].get('boot_changed'):
                current['pending']['phase'] = 'failed'
                self.save(current)
            else:
                # A rejected drain must leave existing instances operable.
                self.save({**current.get('previous', {}), 'last_failure_at': self.s.timestamp()})
                self.discard_backup()
            raise

    def quiesce(self):
        if not self.installation.state().get('current'):
            return
        if self.s.run([str(self.s.AGENT_BIN), 'cvm-check', '--maintenance-contract']) != 'brocade-cvm-maintenance-v1':
            raise self.s.StackError('upgrade Agent to the CVM maintenance contract before switching')
        loaded = self.s.run(['systemctl', 'show', '--value', '-p', 'LoadState', 'brocade-agent.service']) == 'loaded'
        if not loaded:
            raise self.s.StackError('mode switching requires the managed Agent service')
        active = self.s.run(['systemctl', 'show', '--value', '-p', 'ActiveState', 'brocade-agent.service']) == 'active'
        try:
            if active:
                self.s.run(['systemctl', 'stop', 'brocade-agent.service'])
            if self.s.run([str(self.s.AGENT_BIN), 'cvm-check', '--empty', '--config',
                           str(self.installation.base / 'current/generated/cube.json')]) != 'brocade-cvm-empty-v1':
                raise self.s.StackError('node is not confirmed empty; remove all CVM instances before switching')
        except self.s.StackError as error:
            raise self.s.StackError('CVM mode switch could not prove the node empty; delete all CVM instances and confirm cleanup before retrying') from error
        finally:
            if active:
                self.s.run(['systemctl', 'start', 'brocade-agent.service'])

    def commit(self):
        state = self.state()
        mode = self.mode()
        self.check_live(mode)
        if state.get('pending', {}).get('boot_entry'):
            self.s.run(['grub-set-default', state['pending']['boot_entry']])
            state['boot_entry'] = state['pending']['boot_entry']
            self.s.run(['systemctl', 'disable', 'brocade-cvm-resume.service'])
        state.pop('pending', None)
        state.pop('previous', None)
        state.update(effective=mode, completed_at=self.s.timestamp())
        self.save(state)
        self.discard_backup()

    def cancel(self):
        state = self.state()
        if not state.get('pending'):
            raise self.s.StackError('there is no pending virtualization transition')
        previous = state.get('previous', {})
        self.s.run(['grub-editenv', '-', 'unset', 'next_entry'])
        self.s.run(['grub-set-default', previous.get('boot_entry', 'brocade-cvm-native')])
        self.s.run(['systemctl', 'disable', 'brocade-cvm-resume.service'])
        # Keep an explicit failure if the machine already booted another mode.
        if self.installation.state().get('current') and probe() != previous.get('effective', 'kvm'):
            raise self.s.StackError('original kernel selected for the next boot; reboot before resuming the previous CVM mode')
        self.restore_configuration()
        self.save({**previous, 'cancelled_at': self.s.timestamp()})
        self.discard_backup()
