#!/usr/bin/env python3
"""Install the node-only bundle on a fresh KVM host, then run real CVM acceptance.

Only the randomly named VM/container and its private files are changed. Failed
acceptance retains that VM and the inner harness's evidence for safe recovery.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid

SCRIPTS = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SCRIPTS))
import cvm_stack as stack


def run(args, timeout=60):
    try:
        return subprocess.check_output(args, stderr=subprocess.STDOUT, timeout=timeout).decode().strip()
    except subprocess.CalledProcessError as error:
        print(error.output.decode(errors='replace')[-6000:], file=sys.stderr, flush=True)
        raise


def stamp():
    return datetime.now(timezone.utc).isoformat(timespec='seconds')


def remote_boot_id(ssh):
    try:
        response = subprocess.run([*ssh, 'cat /proc/sys/kernel/random/boot_id'], capture_output=True, timeout=10)
    except subprocess.TimeoutExpired:
        return None
    if response.returncode:
        return None
    try:
        return str(uuid.UUID(response.stdout.decode().strip()))
    except ValueError:
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('bundle', 'vm-image', 'agent-binary', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('console-binary', 'agent-test-binary', 'cubecli', 'probe-binary', 'xray-binary', 'lan-inspect-binary'):
        parser.add_argument('--' + name, type=Path)
    parser.add_argument('--bundle-sha256', required=True)
    parser.add_argument('--vm-image-sha256', required=True)
    parser.add_argument('--qemu-image', default='cube-qemu-lab:24.04')
    parser.add_argument('--host-memory-mib', type=int, default=6144,
                        help='outer host RAM; use 1024 for the required small-host acceptance')
    parser.add_argument('--host-cpus', type=int, default=4)
    parser.add_argument('--storage-filesystem', choices=('xfs', 'ext4'), default='xfs')
    parser.add_argument('--installation-only', action='store_true', help='measure OS installation compatibility, idempotency, reboot and service coexistence')
    parser.add_argument('--host-virtualization', choices=('native', 'pvm'), default='native', help='pvm hides VMX/SVM, verifies auto fallback, then manually reboots this disposable host')
    parser.add_argument('--prepare-host-only', action='store_true', help='prepare and retain only this disposable host; run separate real Guest acceptance before publication')
    parser.add_argument('--retain-host-on-success', action='store_true', help='retain only this owned VM after Guest cleanup for an additional mode-switch drill')
    args = parser.parse_args()
    if not 768 <= args.host_memory_mib <= 32768 or not 1 <= args.host_cpus <= 16:
        parser.error('invalid disposable host CPU or memory size')
    if args.host_virtualization == 'pvm' and args.installation_only:
        parser.error('PVM mode requires actual Guest acceptance')
    if not args.installation_only and not args.prepare_host_only and any(getattr(args, key) is None for key in ('console_binary','agent_test_binary','cubecli','probe_binary','xray_binary','lan_inspect_binary')):
        parser.error('full Guest acceptance requires Console, test Agent, cubecli, probe, Xray and LAN inspector')
    if args.prepare_host_only and not args.cubecli:
        parser.error('host preparation requires the read-only cubecli diagnostic')
    backend = 'plain' if args.storage_filesystem == 'ext4' else 'xfs'
    manifest = stack.inspect_archive(args.bundle, args.bundle_sha256)
    if manifest['architecture'] != 'x86_64' or stack.digest(args.vm_image) != args.vm_image_sha256:
        parser.error('this acceptance requires an exact verified x86_64 KVM image and bundle')
    if args.host_virtualization == 'pvm' and not manifest.get('pvm'):
        parser.error('PVM acceptance requires a paired host/Guest kernel bundle')
    manager = SCRIPTS / 'cvm_stack.py'
    if manifest.get('provenance', {}).get('installer_sha256') != stack.digest(manager):
        parser.error('the runtime bundle must name this exact installer source')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    inputs, vm, results = (output / name for name in ('inputs', 'vm', 'results'))
    for directory in (inputs, vm, results): directory.mkdir(mode=0o700)
    for source, target in ((args.bundle, inputs / 'bundle.tar.gz'), (manager, inputs / 'cvm_stack.py'), (args.cubecli, inputs / 'cubecli')):
        if source is not None: shutil.copy2(source, target)
    # Freeze binary inputs before starting containers or remote uploads. A
    # later build must not change the bytes or the final evidence for this run.
    artifacts = {}
    for field in ('console_binary', 'agent_test_binary', 'probe_binary', 'xray_binary', 'agent_binary', 'lan_inspect_binary'):
        source = getattr(args, field)
        if source is None: continue
        target = inputs / field.replace('_', '-')
        shutil.copy2(source, target)
        artifacts[field] = stack.digest(target)
        setattr(args, field, target)
    run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(output / 'host.key')])
    public_key = (output / 'host.key.pub').read_text().strip()
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0)); ssh_port = listener.getsockname()[1]
    task = uuid.uuid4().hex[:12]
    name = 'brocade-cvm-install-' + task
    image_id = run(['docker', 'image', 'inspect', '-f', '{{.Id}}', args.qemu_image])
    state = {'started_at': stamp(), 'start_monotonic': time.monotonic(), 'container': name,
             'bundle_sha256': args.bundle_sha256, 'vm_image_sha256': args.vm_image_sha256,
             'qemu_image_id': image_id, 'status': 'preparing', 'ssh_port': ssh_port,
             'artifacts': artifacts, 'storage_filesystem': args.storage_filesystem, 'host_virtualization': args.host_virtualization}
    def save():
        stack.atomic_json(output / 'execution.json', state)
    save()
    prepare = f'''#!/bin/sh
set -eu
umask 077
trap 'chmod 0644 /lab/results/prepare.log; printf failed > /lab/results/failed; chmod 0644 /lab/results/failed' EXIT
exec > /lab/results/prepare.log 2>&1
modprobe kvm_intel || modprobe kvm_amd
modprobe tun
mkdir -p /var/lib/cvm-install-test /data/cubelet/storage /usr/local/lib/brocade
truncate -s 16G /var/lib/cvm-install-test/storage.img
{'mkfs.ext4 -F -E nodiscard' if backend == 'plain' else 'mkfs.xfs -m reflink=1 -f'} /var/lib/cvm-install-test/storage.img
printf '/var/lib/cvm-install-test/storage.img /data/cubelet/storage {args.storage_filesystem} loop 0 0\\n' >> /etc/fstab
mount /data/cubelet/storage
install -m 0755 /lab/inputs/cvm_stack.py /usr/local/lib/brocade/cvm-stack.py
install -m 0755 /lab/inputs/cubecli /usr/local/lib/brocade/cubecli-test
install -m 0755 /lab/inputs/agent-binary /usr/local/bin/brocade-agent
install -m 0600 /lab/inputs/bundle.tar.gz /root/cvm-install.tar.gz
python3 /usr/local/lib/brocade/cvm-stack.py install --bundle /root/cvm-install.tar.gz --sha256 {args.bundle_sha256}
python3 /usr/local/lib/brocade/cvm-stack.py status > /lab/results/installation.json
ln -s brocade-cvm-cubelet.service /etc/systemd/system/brocade-recovery-cubelet.service
systemctl daemon-reload
python3 - <<'INNER'
import json,pathlib
base=pathlib.Path('/opt/brocade/cvm/current')
config=json.loads((base/'generated/cube.json').read_text())
system=json.loads(pathlib.Path(next(iter(config['image_files'].values()))).read_text())
path=pathlib.Path('/root/cvm-acceptance-system.json')
path.write_text(json.dumps({{'Templates':[{{'ID':'installed-system','Request':system}}]}}));path.chmod(0o600)
INNER
touch /lab/results/ready
chmod 0644 /lab/results/ready /lab/results/installation.json /lab/results/prepare.log
trap - EXIT
'''
    if args.host_virtualization == 'pvm':
        install_line = 'python3 /usr/local/lib/brocade/cvm-stack.py install --bundle /root/cvm-install.tar.gz --sha256 ' + args.bundle_sha256 + '\n'
        before, separator, after = prepare.partition(install_line)
        if not separator: raise RuntimeError('PVM preparation lost its real installer invocation')
        before = before.replace('modprobe kvm_intel || modprobe kvm_amd', '# Native KVM is intentionally unavailable in this owned VM.')
        prepare = before + 'status=0\n' + install_line.rstrip() + ' || status=$?\n' + \
            '[ "$status" = 75 ] || { echo "expected pending-reboot exit 75, got $status"; exit 1; }\n' + \
            'touch /lab/results/awaiting-reboot\nchmod 0644 /lab/results/awaiting-reboot /lab/results/prepare.log\ntrap - EXIT\n'
        (inputs / 'finish.sh').write_text('#!/bin/sh\nset -eu\numask 077\nexec >> /lab/results/prepare.log 2>&1\n' + after)
    (inputs / 'prepare.sh').write_text(prepare)
    if args.installation_only:
        shutil.copy2(Path(__file__).with_name('installation_compat.py'), inputs / 'installation_compat.py')
        (inputs / 'prepare.sh').write_text('''#!/bin/sh
set -eu
umask 077
trap 'chmod 0644 /lab/results/prepare.log; printf failed > /lab/results/failed; chmod 0644 /lab/results/failed' EXIT
exec > /lab/results/prepare.log 2>&1
python3 /lab/inputs/installation_compat.py prepare ''' + args.bundle_sha256 + ' ' + args.storage_filesystem + '''
touch /lab/results/ready
chmod 0644 /lab/results/ready /lab/results/prepare.log /lab/results/compatibility.json
trap - EXIT
''')
    (vm / 'user-data').write_text('''#cloud-config
package_update: ''' + ('false' if args.installation_only else 'true') + '''
package_upgrade: false
''' + ('' if args.installation_only else 'packages: [python3, iproute2, nftables, conntrack, wireguard-tools, util-linux, xfsprogs, kmod, ca-certificates]\n') + '''
users:
  - name: cvm-lab
    groups: sudo
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - ''' + json.dumps(public_key) + '''
runcmd:
  - [mkdir, -p, /lab]
  - [mount, -t, 9p, -o, "trans=virtio,version=9p2000.L", lab, /lab]
  - [sh, /lab/inputs/prepare.sh]
''')
    (vm / 'meta-data').write_text('instance-id: cvm-install-' + task + '\n')
    run(['docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm', '-v', f'{args.vm_image.resolve()}:/base.img:ro', image_id,
         'qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', '/base.img', '/vm/root.qcow2', '48G'])
    run(['docker', 'run', '--rm', '--network', 'none', '-v', f'{vm}:/vm', image_id, 'cloud-localds', '/vm/seed.img', '/vm/user-data', '/vm/meta-data'])
    wrapper = output / 'ssh.sh'
    wrapper.write_text('#!/bin/sh\nset -eu\nport=$1\nshift\nexec ssh -i ' + shlex.quote(str(output / 'host.key'))
        + ' -p "$port" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile='
        + shlex.quote(str(output / 'known-hosts')) + ' -o ConnectTimeout=5 cvm-lab@127.0.0.1 "$@"\n')
    wrapper.chmod(0o700)
    power = output / 'power.py'
    power.write_text('#!/usr/bin/env python3\nimport subprocess\n'
        + 'subprocess.run(' + repr(['docker', 'kill', '--signal', 'KILL', name]) + ',check=True,stdout=subprocess.DEVNULL,timeout=30)\n'
        + 'subprocess.run(' + repr(['docker', 'start', name]) + ',check=True,stdout=subprocess.DEVNULL,timeout=30)\n')
    power.chmod(0o700)
    launched, complete = False, False
    try:
        vm_command = ['docker', 'run', '-d', '--name', name, '--network', 'host', '--device', '/dev/kvm', '--memory', str(args.host_memory_mib+1024)+'m', '--pids-limit', '2048',
             '-v', f'{output}:/lab', '-v', f'{vm}:/vm', '-v', f'{args.vm_image.resolve()}:/base.img:ro', image_id,
             'qemu-system-x86_64', '-accel', 'kvm', '-cpu', 'host,-vmx,-svm' if args.host_virtualization == 'pvm' else 'host', '-smp', str(args.host_cpus), '-m', str(args.host_memory_mib), '-display', 'none',
             '-serial', 'file:/lab/results/serial.log', '-drive', 'file=/vm/root.qcow2,if=virtio,format=qcow2',
             '-drive', 'file=/vm/seed.img,if=virtio,format=raw,readonly=on',
             '-netdev', f'user,id=net0,hostfwd=tcp:127.0.0.1:{ssh_port}-:22', '-device', 'virtio-net-pci,netdev=net0',
             '-fsdev', 'local,id=lab,path=/lab,security_model=none', '-device', 'virtio-9p-pci,fsdev=lab,mount_tag=lab']
        run(vm_command)
        (output / 'vm-command.json').write_text(json.dumps({'command': vm_command},indent=2) + '\n')
        launched = True
        next_progress = time.monotonic()
        pvm_rebooted = False
        while not (results / 'ready').exists():
            if args.host_virtualization == 'pvm' and not pvm_rebooted and (results / 'awaiting-reboot').exists():
                before = remote_boot_id(['sh', str(wrapper), str(ssh_port)])
                if not before: raise RuntimeError('PVM preparation did not expose the actual pre-reboot boot identity')
                pending_code = "import json,pathlib; s=json.loads(pathlib.Path('/opt/brocade/cvm/host.json').read_text()); assert s['pending']['phase']=='awaiting_reboot' and s['pending']['mode']=='pvm'; flags=pathlib.Path('/proc/cpuinfo').read_text().split(); assert 'vmx' not in flags and 'svm' not in flags; print(json.dumps(s))"
                proof = run(['sh', str(wrapper), str(ssh_port), 'sudo python3 -c ' + shlex.quote(pending_code)])
                state['pvm_before_reboot'] = json.loads(proof);save()
                subprocess.run(['sh', str(wrapper), str(ssh_port), 'sudo systemctl reboot'],capture_output=True,timeout=20)
                deadline = time.monotonic() + 240
                while True:
                    observed = remote_boot_id(['sh', str(wrapper), str(ssh_port)])
                    if observed and observed != before: break
                    if time.monotonic() > deadline: raise RuntimeError('paired PVM host kernel did not boot')
                    time.sleep(1)
                deadline = time.monotonic() + 180
                while True:
                    response = subprocess.run(['sh', str(wrapper), str(ssh_port), 'sudo systemctl show brocade-cvm-resume.service --property=ActiveState --property=SubState --property=Result'],capture_output=True,text=True,timeout=15)
                    if response.returncode == 0 and 'ActiveState=inactive' in response.stdout and 'Result=success' in response.stdout:
                        state_check = subprocess.run(['sh',str(wrapper),str(ssh_port),"sudo python3 -c \"import json,pathlib; s=json.loads(pathlib.Path('/opt/brocade/cvm/host.json').read_text()); assert s.get('effective')=='pvm' and not s.get('pending')\""],capture_output=True,timeout=15)
                        if state_check.returncode == 0: break
                    if response.returncode == 0 and 'ActiveState=failed' in response.stdout: raise RuntimeError('PVM boot resume failed; retain the owned VM for diagnosis')
                    if time.monotonic() > deadline: raise RuntimeError('PVM boot resume did not complete')
                    time.sleep(1)
                verify_code = "import json,pathlib,platform; s=json.loads(pathlib.Path('/opt/brocade/cvm/host.json').read_text()); assert s['effective']=='pvm' and not s.get('pending'); assert pathlib.Path('/sys/module/kvm_pvm').is_dir(); c=json.loads(pathlib.Path('/opt/brocade/cvm/current/generated/cube.json').read_text()); assert c['virtualization_mode']=='pvm'; print(json.dumps({'kernel':platform.release(),'mode':s,'guest_kernel_mode':c['virtualization_mode']}))"
                state['pvm_after_reboot'] = json.loads(run(['sh',str(wrapper),str(ssh_port),'sudo python3 -c ' + shlex.quote(verify_code)]));save()
                subprocess.run(['sh', str(wrapper), str(ssh_port), 'sudo mkdir -p /lab/results && sudo sh -s'], input=(inputs / 'finish.sh').read_bytes(), capture_output=True, check=True, timeout=60)
                installation = run(['sh',str(wrapper),str(ssh_port),'sudo cat /lab/results/installation.json'])
                (results / 'installation.json').write_text(installation + '\n')
                (results / 'ready').touch()
                pvm_rebooted = True

            if (results / 'failed').exists(): raise RuntimeError('fresh-host CVM installation failed; inspect private prepare.log')
            if time.monotonic() - state['start_monotonic'] > 1200: raise RuntimeError('fresh host preparation exceeded its bounded network/setup deadline')
            if time.monotonic() >= next_progress:
                print(f'[{stamp()}] fresh KVM preparation: {time.monotonic()-state["start_monotonic"]:.0f}s', flush=True)
                next_progress = time.monotonic() + 30
            time.sleep(1)
        state['status'] = 'installed'; save()
        if args.prepare_host_only:
            state['status'] = 'prepared-retained';save()
            return
        if args.installation_only:
            compatibility = json.loads((results / 'compatibility.json').read_text())
            state['compatibility'] = compatibility
            if compatibility['installation'] == 'installed':
                # Reuse the recorded boot identity. The first SSH connection
                # can emit a host-key warning on stderr; merging it with a UUID
                # falsely makes the original boot appear to be a new one.
                before = str(uuid.UUID(compatibility['facts']['boot_id']))
                subprocess.run(['sh', str(wrapper), str(ssh_port), 'sudo systemctl reboot'], capture_output=True, timeout=20)
                deadline = time.monotonic() + 180
                while True:
                    boot_id = remote_boot_id(['sh', str(wrapper), str(ssh_port)])
                    if boot_id and boot_id != before: break
                    if time.monotonic() > deadline: raise RuntimeError('installation compatibility reboot did not return')
                    time.sleep(1)
                # sshd may briefly restart during boot. Retry only transport
                # failures; a verifier assertion must retain the failed VM.
                transport_deadline = time.monotonic() + 30
                while True:
                    check = subprocess.run(['sh', str(wrapper), str(ssh_port), 'sudo python3 /usr/local/lib/brocade/cvm-install-compat.py verify'],
                                           capture_output=True, timeout=90)
                    if check.returncode != 255 or time.monotonic() >= transport_deadline: break
                    time.sleep(1)
                if check.returncode:
                    print((check.stdout + check.stderr).decode(errors='replace')[-6000:], file=sys.stderr, flush=True)
                    raise RuntimeError('installation compatibility reboot verifier failed')
                proof = check.stdout.decode().strip()
                state['compatibility']['after_reboot'] = json.loads(proof)
                state['status'] = 'passed'
            else:
                state['status'] = 'rejected' if compatibility['installation'] == 'rejected' else 'failed'
            # This mode never allocates an inner Guest; all VM contents belong
            # to this run, so diagnosis can be retained before removing the VM.
            complete = True
            return
        false_binary = output / 'guest-false'
        with false_binary.open('wb') as stream:
            subprocess.run(['sh', str(wrapper), str(ssh_port), 'cat /usr/bin/false'], stdout=stream, stderr=subprocess.PIPE, check=True, timeout=30)
        with tempfile.TemporaryDirectory(dir=output, prefix='variants-') as temporary:
            tree = Path(temporary)
            stack.inspect_archive(args.bundle, args.bundle_sha256, tree)
            (tree / 'manifest.json').unlink()
            (tree / 'runtime/acceptance-release').write_text('compatible activation exercise\n')
            good = inputs / 'good.tar.gz'
            stack.pack(tree, good, manifest['architecture'], manifest['source_sha256'], manifest['min_glibc'], {'acceptance_variant': 'compatible'}, storage_backends=manifest.get('storage_backends', ['xfs']), pvm=manifest.get('pvm'))
            shutil.copyfile(false_binary, tree / 'runtime/Cubelet/bin/cubelet')
            (tree / 'runtime/Cubelet/bin/cubelet').chmod(0o755)
            bad = inputs / 'bad.tar.gz'
            stack.pack(tree, bad, manifest['architecture'], manifest['source_sha256'], manifest['min_glibc'], {'acceptance_variant': 'fails-readiness'}, storage_backends=manifest.get('storage_backends', ['xfs']), pvm=manifest.get('pvm'))
        good_sha, bad_sha = stack.digest(good), stack.digest(bad)
        exercise = output / 'stack-exercise.py'
        exercise.write_text(EXERCISE.replace('@WRAPPER@', repr(str(wrapper))).replace('@GOOD@', repr(good_sha))
            .replace('@BAD@', repr(bad_sha)).replace('@ORIGINAL@', repr(args.bundle_sha256)).replace('@LOG@', repr(str(results / 'stack-exercise.log'))))
        exercise.chmod(0o700)
        child = [sys.executable, str(Path(__file__).with_name('kvm_acceptance.py')), '--console-binary', str(args.console_binary.resolve()),
                 '--agent-test-binary', str(args.agent_test_binary.resolve()),
                 '--ssh-helper', str(wrapper), '--ssh-port', str(ssh_port), '--cube-config', '/root/cvm-acceptance-system.json',
                 '--cube-socket', '/run/cubelet/cubelet.sock', '--cubecli', '/usr/local/lib/brocade/cubecli-test',
                 '--storage-backend', backend, '--virtualization-mode', 'pvm' if args.host_virtualization == 'pvm' else 'kvm', '--network-inspection', 'http://127.0.0.1:8080/v1/network/taps', '--source-image-id', 'installed-system',
                 '--output', str(results / 'acceptance'), '--remote-nics', '--smoke',
                 '--network-probe-binary', str(args.probe_binary.resolve()), '--xray-binary', str(args.xray_binary.resolve())]

        subprocess.run(child, check=True)
        result = json.loads((results / 'acceptance/result.json').read_text())
        if not result['guestCleanupConfirmed']: raise RuntimeError('inner Guest cleanup is not proven')
        complete = True
        state['status'] = 'passed'
    finally:
        state.update(ended_at=stamp(), duration_seconds=round(time.monotonic()-state['start_monotonic'], 3))
        if launched:
            with (results / 'qemu.log').open('w') as log:
                subprocess.run(['docker', 'logs', name], stdout=log, stderr=subprocess.STDOUT, timeout=30)
            if complete and not args.retain_host_on_success:
                run(['docker', 'rm', '-f', name])
            elif complete:
                state['status'] = 'passed-retained'
            elif state['status'] != 'prepared-retained':
                state['status'] = 'failed-retained'
        save()
        print(json.dumps({key: state[key] for key in ('status', 'started_at', 'ended_at', 'duration_seconds')}), flush=True)


EXERCISE = r'''#!/usr/bin/env python3
import json,subprocess,sys
wrapper=@WRAPPER@
port,prefix,instance=sys.argv[1:]
code=r"""
import json,pathlib,subprocess,sys
prefix,instance,good,bad,original=json.loads(sys.stdin.read())
manager='/usr/local/lib/brocade/cvm-stack.py'
def cli(*args,ok=True):
 result=subprocess.run(['python3',manager,*args],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=180)
 if (result.returncode==0)!=ok:raise RuntimeError('stack activation outcome mismatch')
 return result.stdout
def current():return json.loads(cli('status'))['current']
assert current()==original
record=json.loads((pathlib.Path(prefix)/'runtime'/(instance+'.json')).read_text())
backend=json.loads(pathlib.Path('/opt/brocade/cvm/current/generated/cube.json').read_text())['storage_backend']
volume=record['objects'][0]['name'];path=pathlib.Path('/data/cubelet/storage')/backend/'objects/volumes'/volume/volume
inode=path.stat().st_ino
for name in ['good','bad']:
 subprocess.run(['install','-m','0600','/lab/inputs/'+name+'.tar.gz','/root/cvm-'+name+'.tar.gz'],check=True)
cli('install','--bundle','/root/cvm-good.tar.gz','--sha256',good);assert current()==good and path.stat().st_ino==inode
cli('install','--bundle','/root/cvm-bad.tar.gz','--sha256',bad,ok=False);assert current()==good and path.stat().st_ino==inode
cli('rollback');assert current()==original and path.stat().st_ino==inode
print(json.dumps({'upgrade':True,'failed_activation_recovered':True,'rollback':True,'root_inode':inode}))
"""
import shlex
result=subprocess.run(['sh',wrapper,port,'sudo python3 -c '+shlex.quote(code)],input=json.dumps([prefix,instance,@GOOD@,@BAD@,@ORIGINAL@]).encode(),capture_output=True,timeout=600)
with open(@LOG@,'wb') as log:log.write(result.stdout+result.stderr)
sys.exit(result.returncode)
'''


if __name__ == '__main__':
    main()
