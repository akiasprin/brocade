"""Real lifecycle checks shared by the explicitly opted-in KVM acceptance."""
import json
import shlex
import time


def install_services(py, remote, prefix, unit, network_lab):
    # Replace only this run's transient units, before creating any Guest.
    remote('sudo systemctl stop ' + unit + (' ' + unit + '-echo' if network_lab else ''))
    remote('sudo systemctl reset-failed ' + unit + (' ' + unit + '-echo' if network_lab else ''), check=False)
    script = r'''
import pathlib,json,subprocess
base=pathlib.Path(__file__).parent;c=json.loads((base/'network-lab.json').read_text())
def ip(*a):subprocess.run(['ip',*a],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
for b in c['bridges']:
 p=pathlib.Path('/sys/class/net')/b['bridge']
 if p.exists():assert (p/'ifalias').read_text().strip()=='brocade-acceptance:'+c['task']
 else:ip('link','add',b['bridge'],'type','bridge');ip('link','set','dev',b['bridge'],'alias','brocade-acceptance:'+c['task'])
 ip('addr','replace',b['ipv4_gateway']+'/24','dev',b['bridge']);ip('-6','addr','replace',b['ipv6_gateway']+'/64','dev',b['bridge'],'nodad');ip('link','set','dev',b['bridge'],'up')
ip('addr','replace',c['probe4']+'/32','dev','lo');ip('-6','addr','replace',c['probe6']+'/128','dev','lo','nodad')
'''
    files = {prefix + '/restore-net.py': script}
    dependencies = 'brocade-recovery-cubelet.service'
    if network_lab:
        files['/etc/systemd/system/' + unit + '-network.service'] = '[Unit]\nAfter=network-online.target\nBefore=' + unit + '.service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/usr/bin/python3 ' + prefix + '/restore-net.py\n[Install]\nWantedBy=multi-user.target\n'
        files['/etc/systemd/system/' + unit + '-echo.service'] = '[Unit]\nAfter=' + unit + '-network.service\nRequires=' + unit + '-network.service\n[Service]\nExecStart=/usr/bin/python3 ' + prefix + '/echo.py\nRestart=always\n[Install]\nWantedBy=multi-user.target\n'
        dependencies += ' ' + unit + '-network.service'
    files['/etc/systemd/system/' + unit + '.service'] = '[Unit]\nAfter=' + dependencies + '\nRequires=' + dependencies + '\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n'
    py('import json,sys,pathlib;files=json.load(sys.stdin);[(pathlib.Path(p).write_text(s),pathlib.Path(p).chmod(0o600)) for p,s in files.items()]', json.dumps(files).encode())
    remote('sudo systemctl daemon-reload && sudo systemctl enable --now ' + unit + (' ' + unit + '-network ' + unit + '-echo' if network_lab else ''))


def cleanup_services(py, remote, unit, network_lab):
    names = [unit] + ([unit + '-network', unit + '-echo'] if network_lab else [])
    remote('sudo systemctl disable --now ' + ' '.join(names), check=False)
    py('import pathlib;[(pathlib.Path("/etc/systemd/system")/(n+".service")).unlink(missing_ok=True) for n in ' + repr(names) + ']')
    remote('sudo systemctl daemon-reload')


def verify(guest, py, remote, prefix, created, record, operation, wait_instance, converge, power_cycle, network_lab):
    def physical_root_inode():
        return int(py('import pathlib,json;r=json.loads(pathlib.Path(' + repr(prefix + '/runtime/' + created['desired']['id'] + '.json') + ').read_text());v=r["objects"][0]["name"];c=json.loads(pathlib.Path(' + repr(prefix + '/config.json') + ').read_text());print((pathlib.Path("/data/cubelet/storage")/c["storage_backend"]/"objects/volumes"/v/v).stat().st_ino)'))
    original_inode = physical_root_inode()
    original_volume = created['report']['volume_id']
    original_boot = guest('cat /proc/sys/kernel/random/boot_id')
    marker = guest('cat ~/brocade-acceptance.txt')
    host_key = guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256')
    before_addresses = {n['id']: sorted(n['addresses']) for n in created['report']['interfaces']}
    operation('restart'); created = wait_instance()
    assert created['report']['volume_id'] == original_volume
    assert physical_root_inode() == original_inode
    assert guest('cat /proc/sys/kernel/random/boot_id') != original_boot
    assert guest('cat ~/brocade-acceptance.txt') == marker
    boot = guest('cat /proc/sys/kernel/random/boot_id')
    converge(); converge()
    assert guest('cat /proc/sys/kernel/random/boot_id') == boot
    assert guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256') == host_key
    record('durable restart changes Guest boot once and preserves original root, SSH identity and data')
    operation('stop'); created = wait_instance()
    assert created['report']['power'] == 'stopped' and created['report']['volume_retained']
    assert created['report']['network_absent'] and created['report']['runtime_id'] is None
    record('shutdown removes the actual runtime and devices while retaining the original root')
    if network_lab:
        removed = next(nic for nic in created['desired']['spec']['interfaces'] if nic['source_id'] == 'business-b')
        removed_id = removed['id']
        operation('detach', interface_id=removed_id); created = wait_instance()
        assert created['report']['power'] == 'stopped'
        assert removed_id in created['report']['detached_interfaces']
        assert created['report']['network_absent'] and created['report']['volume_retained']
        code = 'import json,pathlib;r=json.loads(pathlib.Path(' + repr(network_lab['native_state_dir'] + '/' + created['desired']['id'] + '.json') + ').read_text());n=r["nics"].get(' + repr(removed_id) + ');assert n is None or (n["removed"] and n["ifindex"]==0)'
        py(code)
        converge(); converge()
        operation('start'); created = wait_instance()
        assert created['report']['volume_id'] == original_volume and physical_root_inode() == original_inode
        assert guest('cat ~/brocade-acceptance.txt') == marker
        assert guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256') == host_key
        assert {n['id']: sorted(n['addresses']) for n in created['report']['interfaces']} == {
            key: value for key, value in before_addresses.items() if key != removed_id}
        record('shutdown, business NIC detach, exact cleanup receipt and subsequent boot preserve original disk, data, SSH and remaining addresses', detached_interface=removed_id)
        operation('stop'); created = wait_instance()
        operation('attach', interface={key: removed[key] for key in ('source_id','purpose','default_ipv4','default_ipv6')})
        created = wait_instance()
        assert created['report']['power'] == 'stopped'
        new_id = next(nic['id'] for nic in created['desired']['spec']['interfaces'] if nic['source_id'] == 'business-b')
        assert new_id != removed_id
    operation('configure', name=created['desired']['spec']['name'] + '-edited', vcpus=2, memory_bytes=335544320)
    created = wait_instance(); assert created['report']['power'] == 'stopped'
    operation('start'); created = wait_instance()
    assert created['report']['volume_id'] == original_volume and guest('cat ~/brocade-acceptance.txt') == marker
    assert physical_root_inode() == original_inode
    assert int(guest("awk '/^processor/ {n++} END {print n}' /proc/cpuinfo")) == 2
    assert 262144 < int(guest("awk '/MemTotal:/ {print $2}' /proc/meminfo")) <= 327680
    current_addresses = {n['id']: sorted(n['addresses']) for n in created['report']['interfaces']}
    if network_lab:
        assert {key: current_addresses[key] for key in before_addresses if key != removed_id} == {
            key: value for key, value in before_addresses.items() if key != removed_id}
        assert new_id in current_addresses and removed_id not in current_addresses
        before_addresses = current_addresses
        record('a business NIC reserved while stopped attaches on boot without resurrecting the removed identity')
    else:
        assert current_addresses == before_addresses
    record('CPU and memory reconfiguration boots two real vCPUs and 320 MiB on the same root and NIC addresses')
    parent_boot = remote('cat /proc/sys/kernel/random/boot_id').stdout.decode().strip()
    guest('sync')
    boot = guest('cat /proc/sys/kernel/random/boot_id')
    power_cycle()
    for _ in range(120):
        if remote('sudo systemctl is-active --quiet brocade-recovery-cubelet.service', check=False, timeout=10).returncode == 0:
            break
        time.sleep(1)
    else:
        raise RuntimeError('Cubelet did not return after selected host power loss')
    assert remote('cat /proc/sys/kernel/random/boot_id').stdout.decode().strip() != parent_boot
    created = wait_instance()
    assert created['report']['volume_id'] == original_volume
    assert physical_root_inode() == original_inode
    assert guest('cat /proc/sys/kernel/random/boot_id') != boot
    assert guest('cat ~/brocade-acceptance.txt') == marker
    assert guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256') == host_key
    assert {n['id']: sorted(n['addresses']) for n in created['report']['interfaces']} == before_addresses
    record('abrupt outer KVM power loss recovers the original root, acknowledged data, SSH identity and all NIC addresses', physical_root_inode=original_inode)
    if network_lab:
        ports = json.loads(py('import pathlib;print(pathlib.Path(' + repr(prefix + '/echo-ports.json') + ').read_text())'))
        for protocol, port in ports.items():
            family = 6 if protocol.endswith('6') else 4
            target = network_lab['probe6' if family == 6 else 'probe4']
            address = '[' + target + ']' if family == 6 else target
            assert 'after-power-loss' in guest('~/cvm-probe ' + protocol + ' ' + shlex.quote(address + ':' + str(port)) + ' after-power-loss')
        record('all configured IPv4 and IPv6 TCP and UDP paths work after host power recovery')
    operation('reinstall', image_id=created['desired']['spec']['image']['id'], ssh_public_key=created['desired']['spec']['ssh_public_key'])
    # Reinstallation intentionally rotates SSH identity. Forget only this run's
    # known-host entry after the API has accepted that destructive intention.
    remote('sudo rm -f ' + shlex.quote(prefix + '/guest.known-' + created['desired']['id']))
    created = wait_instance()
    assert guest('test ! -e ~/brocade-acceptance.txt && printf reset') == 'reset'
    assert guest('ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256') != host_key
    assert {n['id']: sorted(n['addresses']) for n in created['report']['interfaces']} == before_addresses
    new_boot = guest('cat /proc/sys/kernel/random/boot_id')
    guest('printf reinstall-once > ~/brocade-acceptance.txt; sync')
    converge(); converge()
    assert guest('cat ~/brocade-acceptance.txt') == 'reinstall-once' and guest('cat /proc/sys/kernel/random/boot_id') == new_boot
    record('explicit reinstall resets root data and SSH identity once while preserving CPU, memory and NIC addresses')
    return created
