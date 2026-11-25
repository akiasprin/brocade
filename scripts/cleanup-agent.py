#!/usr/bin/env python3
"""Preview or remove a systemd Brocade Agent and its owned Linux network resources."""

import argparse
import fcntl
import hashlib
import ipaddress
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path

UNITS = ('brocade-agent.service', 'brocade-host-terminal.service', 'brocade-cvm-cubelet.service')
# Exact public identities from the Agent's network executors. Never match an arbitrary prefix.
TABLES = {
    ('inet', 'brocade'),
    ('inet', 'brocade_conntrack'),
    ('inet', 'brocade_hy2_port_hop'),
    ('inet', 'brocade_hy2_nat'),
    ('ip', 'brocade_hy2_transport'),
    ('ip6', 'brocade_hy2_transport'),
    ('ip', 'brocade_cvm_entries'),
    ('ip', 'brocade_cvm_entry_withdraw'),
    ('inet', 'brocade_entry_phantun'),
    ('inet', 'brocade_host_output'),
    ('inet', 'brocade_cvm_nic_fences'),
}
TABLE_OWNERS = {
    ('inet', 'brocade_host_output'): 'brocade-native-output-v1',
    ('inet', 'brocade_cvm_nic_fences'): 'brocade-cvm-nics-v2',
    ('ip', 'brocade_cvm_entry_withdraw'): 'brocade-entry-withdraw-v1',
}
ALIASES = (
    'brocade-nic:',
    'brocade-host:',
    'brocade-chain:',
    'brocade-entry:',
    'brocade-entry-phantun:',
    'brocade-binding:',
)
CUBE_PINS = frozenset(
    {
        'allow_out',
        'allow_out_v2',
        'dns_allow',
        'dns_query_scratch',
        'dns_query_state',
        'dns_response_state',
        'tungrp_to_tuns',
        'cube_lan_ep_v1',
        'cube_lan_pfx_v1',
        'cube_lan_ct_v1',
        'cube_lan_stat_v1',
        'cube_q_ep_v1',
        'cube_q_cfg_v1',
        'cube_q_count_v1',
        'cube_q_flow_v1',
        'cube_q_retire_v1',
        'allow_out_v3',
        'deny_out',
        'direct_neigh',
        'dns_allow_v2',
        'dns_finish',
        'dns_handle_response_prog',
        'dns_parse_chunk',
        'dns_query_track',
        'dns_response_finish_prog',
        'dns_rev_chunk',
        'dns_tail_calls',
        'egress_sessions',
        'from_cube',
        'from_envoy',
        'from_world',
        'ifindex_to_mvmmeta',
        'ingress_sessions',
        'local_port_mapping',
        'mvmip_to_ifindex',
        'primary_chain_frag',
        'primary_chain_routes',
        'primary_delivery',
        'remote_port_mapping',
        'snat_iplist',
    }
)
CUBE_PROGRAMS = frozenset({'cubevs_lan_in', 'cubevs_lan_out', 'cube_q_in', 'cube_q_out'})


class CleanupError(RuntimeError):
    pass


def timestamp():
    return datetime.now(UTC).isoformat(timespec='seconds')


class Runner:
    def __init__(self):
        self.actions = []

    def run(self, *args, missing_ok=False):
        try:
            result = subprocess.run(args, check=False, capture_output=True, text=True, timeout=60)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise CleanupError(f'Cannot execute {args[0]}: {type(error).__name__}') from None
        if result.returncode and not missing_ok:
            # Do not copy command output: service configuration can contain credentials.
            raise CleanupError(f'{args[0]} failed (exit {result.returncode}): {shlex.join(args[1:])}')
        return result

    def text(self, *args):
        return self.run(*args).stdout

    def json(self, *args):
        try:
            return json.loads(self.text(*args))
        except (ValueError, TypeError):
            raise CleanupError(f'Invalid JSON from {args[0]}') from None

    def change(self, *args):
        self.run(*args)
        self.actions.append({'time': timestamp(), 'argv': list(args)})


def owned_table(table):
    family, name, owner = table['family'], table['name'], table.get('comment', '')
    if (family, name) in TABLES:
        return (family, name) not in TABLE_OWNERS or owner == TABLE_OWNERS[(family, name)]
    if family == 'inet' and re.fullmatch(r'brocade_chain_[0-9a-f]{12}', name):
        return owner.startswith('brocade-chain:')
    if family == 'inet' and re.fullmatch(r'bcn[cp][0-9a-f]{12}', name):
        return owner.startswith('brocade-nic:')
    if family == 'bridge' and re.fullmatch(r'bcg[0-9a-f]{12}', name):
        return owner.startswith('brocade-nic:')
    return False


def parse_iptables(source):
    table = None
    rules = []
    for line in source.splitlines():
        if line.startswith('*'):
            table = line[1:]
        elif line == 'COMMIT':
            table = None
        elif line.startswith('-A ') and table:
            rules.append((table, shlex.split(line)))
    return rules


def cube_rule(table, args, router, addresses):
    """Only the five exact shapes emitted by CubeRouter, with explicit NAT identities."""
    # iptables-save adds protocol match modules and sorts conntrack state names.
    clean = list(args)
    for protocol in ('tcp', 'udp', 'icmp'):
        if '-m' in clean and clean[clean.index('-m') + 1] == protocol:
            index = clean.index('-m')
            del clean[index : index + 2]
    for address in addresses:
        source = str(ipaddress.ip_address(address)) + '/32'
        if table == 'filter':
            if clean == ['-A', 'FORWARD', '-i', router, '-s', source, '-j', 'ACCEPT']:
                return True
            expected = ['-A', 'FORWARD', '-o', router, '-d', source, '-m', 'conntrack', '--ctstate']
            if clean[:9] == expected and len(clean) == 12 and clean[10:] == ['-j', 'ACCEPT']:
                return set(clean[9].split(',')) == {'ESTABLISHED', 'RELATED'}
        if table != 'nat':
            continue
        base = ['-A', 'POSTROUTING', '-s', source, '!', '-o', router, '-p']
        for protocol in ('tcp', 'udp', 'icmp'):
            prefix = base + [protocol, '-j', 'MASQUERADE']
            if clean == prefix and protocol != 'tcp':
                return True
            if protocol == 'tcp' and clean[:11] == prefix and len(clean) == 13 and clean[11] == '--to-ports':
                match = re.fullmatch(r'(\d+)[-:]65535', clean[12])
                if match and 0 < int(match[1]) < 65535:
                    return True
    return False


def owned_rule(table, args, router, addresses, nat_sources=(), interfaces=()):
    if '--comment' in args:
        index = args.index('--comment')
        if index + 1 < len(args) and args[index + 1] == 'brocade-entry-phantun':
            return True
    # VPN Gate shared rules have no comments; require explicit source/interface claims.
    for source in nat_sources:
        if table == 'nat' and args == ['-A', 'POSTROUTING', '-s', str(source), '-j', 'MASQUERADE']:
            return True
    for interface in interfaces:
        if table == 'filter' and args == ['-A', 'FORWARD', '-i', interface, '-j', 'ACCEPT']:
            return True
        prefix = ['-A', 'FORWARD', '-o', interface, '-m', 'conntrack', '--ctstate']
        if table == 'filter' and args[:7] == prefix and len(args) == 10 and args[8:] == ['-j', 'ACCEPT']:
            return set(args[7].split(',')) == {'RELATED', 'ESTABLISHED'}
    return cube_rule(table, args, router, addresses)


def number(value):
    if isinstance(value, int):
        return value
    try:
        return int(str(value), 16 if str(value).startswith('0x') else 10)
    except ValueError:
        return -1


def policy_table(table):
    return table >> 16 == 0xB8 or table >> 24 == 0xB4


class Network:
    def __init__(self, runner, args):
        self.r = runner
        self.args = args
        self.hy2_owner = hashlib.sha256(os.fsencode(args.state_dir.resolve())).hexdigest()

    def owned_link(self, link):
        alias = link.get('ifalias', '')
        return alias.startswith(ALIASES) or alias == self.hy2_owner or link['ifname'] in self.args.interface

    def inventory(self):
        links = self.r.json('ip', '-d', '-j', 'link', 'show')
        tables = [x['table'] for x in self.r.json('nft', '-j', 'list', 'tables')['nftables'] if 'table' in x]
        # Some nft versions omit table comments from the table listing.
        for index, table in enumerate(tables):
            if (table['family'], table['name']) in TABLE_OWNERS or re.fullmatch(
                r'(?:brocade_chain_|bcn[cp]|bcg)[0-9a-f]{12}', table['name']
            ):
                full = self.r.json('nft', '-j', 'list', 'table', table['family'], table['name'])
                tables[index] = next(x['table'] for x in full['nftables'] if 'table' in x)
        selected = [table for table in tables if owned_table(table)]
        conflicts = [
            table for table in tables if (table['family'], table['name']) in TABLE_OWNERS and not owned_table(table)
        ]
        firewalls = []
        for tool in ('iptables', 'ip6tables', 'iptables-legacy', 'ip6tables-legacy'):
            if not shutil.which(tool + '-save'):
                continue
            rules = parse_iptables(self.r.text(tool + '-save'))
            firewalls.append(
                {
                    'tool': tool,
                    'rules': rules,
                    'owned': [
                        (t, a)
                        for t, a in rules
                        if owned_rule(
                            t,
                            a,
                            self.args.cube_router,
                            self.args.cube_nat_ip,
                            self.args.nat_source,
                            self.args.interface,
                        )
                    ],
                }
            )
        namespaces = []
        unclaimed = []
        for namespace in self.r.json('ip', '-j', 'netns', 'list'):
            name = namespace['name']
            candidate = (
                bool(re.fullmatch(r'bcn-[0-9a-f]{12}-[cp]', name)) or name == 'brocade-hy2-' + self.hy2_owner[:12]
            )
            if not candidate and name not in self.args.namespace:
                if name.startswith('brocade-vg'):
                    unclaimed.append(name)
                continue
            members = self.r.json('ip', 'netns', 'exec', name, 'ip', '-d', '-j', 'link', 'show')
            owned = [link for link in members if link['ifname'] != 'lo']
            if name in self.args.namespace or (owned and all(self.owned_link(link) for link in owned)):
                namespaces.append({'name': name, 'inode': Path('/run/netns', name).stat().st_ino})
            else:
                unclaimed.append(name)
        devices = {row['ifname'] for row in links if self.owned_link(row)}
        routes = []
        for family in ('-4', '-6'):
            rows = self.r.json('ip', family, '-j', 'route', 'show', 'table', 'all')
            candidates = {number(row.get('table')) for row in rows if row.get('dev') in devices}
            candidates |= {number(row.get('table')) for row in rows if policy_table(number(row.get('table')))}
            for table in sorted(candidates):
                if not policy_table(table):
                    continue
                group = [row for row in rows if number(row.get('table')) == table]
                if all(
                    row.get('dev') in devices
                    or (row.get('type') == 'unreachable' and str(row.get('protocol')) in ('186', 'bgp'))
                    for row in group
                ):
                    routes.append((family, table))
        return {
            'tables': selected,
            'firewalls': firewalls,
            'links': [row for row in links if self.owned_link(row)],
            'namespaces': namespaces,
            'unclaimed_namespaces': unclaimed,
            'route_tables': routes,
            'conflicting_tables': conflicts,
        }

    def clear(self, plan):
        if plan.get('conflicting_tables'):
            raise CleanupError('A named nft table has a conflicting ownership comment; retained')
        for namespace in plan['namespaces']:
            name = namespace['name']
            if self.r.text('ip', 'netns', 'pids', name).strip():
                raise CleanupError(f'Namespace {name} still has processes; left intact')
            if Path('/run/netns', name).stat().st_ino != namespace['inode']:
                raise CleanupError(f'Namespace identity changed: {name}')
        for table in plan['tables']:
            self.r.change('nft', 'delete', 'table', table['family'], table['name'])
        for firewall in plan['firewalls']:
            for table, args in firewall['owned']:
                # nft and legacy frontends can expose the same rule; -C makes a repeat harmless.
                check = self.r.run(firewall['tool'], '-w', '5', '-t', table, '-C', *args[1:], missing_ok=True)
                if check.returncode == 0:
                    self.r.change(firewall['tool'], '-w', '5', '-t', table, '-D', *args[1:])
                elif check.returncode != 1:
                    raise CleanupError(f'Cannot inspect shared firewall rule: {firewall["tool"]}')
        for namespace in plan['namespaces']:
            self.r.change('ip', 'netns', 'delete', namespace['name'])
        for link in plan['links']:
            current = next(
                (x for x in self.r.json('ip', '-d', '-j', 'link', 'show') if x['ifindex'] == link['ifindex']), None
            )
            if current is None:
                continue  # Deleting a netns can asynchronously remove the root veth peer.
            if current['ifname'] != link['ifname'] or current.get('ifalias') != link.get('ifalias'):
                raise CleanupError('Interface identity changed during cleanup')
            result = self.r.run('ip', 'link', 'delete', 'dev', link['ifname'], missing_ok=True)
            if result.returncode and any(
                x['ifindex'] == link['ifindex'] for x in self.r.json('ip', '-j', 'link', 'show')
            ):
                raise CleanupError(f'Cannot remove owned interface {link["ifname"]}')
        for family, table in plan['route_tables']:
            for rule in self.r.json('ip', family, '-j', 'rule', 'show'):
                if (
                    number(rule.get('table')) == table
                    and number(rule.get('fwmark')) == table
                    and rule.get('priority') in (28800, 31650)
                ):
                    self.r.change(
                        'ip',
                        family,
                        'rule',
                        'del',
                        'priority',
                        str(rule['priority']),
                        'fwmark',
                        str(table),
                        'lookup',
                        str(table),
                    )
            self.r.change('ip', family, 'route', 'flush', 'table', str(table))
        after = self.inventory()
        if any(after[key] for key in ('tables', 'links', 'namespaces', 'route_tables')) or any(
            x['owned'] for x in after['firewalls']
        ):
            raise CleanupError('Owned network resources remain after cleanup')
        for old in plan['firewalls']:
            expected = Counter(
                (table, tuple(args)) for table, args in old['rules'] if (table, args) not in old['owned']
            )
            new = next(x for x in after['firewalls'] if x['tool'] == old['tool'])
            if expected != Counter((table, tuple(args)) for table, args in new['rules']):
                raise CleanupError('Unrelated firewall rules changed during cleanup; inspect the report')


def safe_path(path):
    if not path.is_absolute() or '..' in path.parts or path == Path('/') or path.resolve() != path:
        raise CleanupError(f'Refusing unsafe or symbolic path: {path}')
    return path


def validate_directories(args):
    for path in (args.state_dir, args.config_dir, args.bin_dir):
        safe_path(path)
    for path in (args.state_dir, args.config_dir):
        if str(path) in (
            '/etc/brocade',
            '/var/lib/brocade',
            '/opt/brocade',
            '/var/cache/brocade',
            '/usr/local/lib/brocade',
        ):
            raise CleanupError('Shared Console/runtime directories cannot be used as Agent data roots')
        if not any(part.startswith('brocade') for part in path.parts[1:]):
            raise CleanupError('Custom state/configuration paths must include a brocade-prefixed directory')
    directories = (args.state_dir, args.config_dir, args.bin_dir)
    if any(a == b or a in b.parents or b in a.parents for i, a in enumerate(directories) for b in directories[i + 1 :]):
        raise CleanupError('Binary, state and configuration paths must not overlap')


def owned_cgroup(group, units):
    return any(unit in row.split('/') for row in group.splitlines() for unit in units)


def process_inventory(units=UNITS):
    owned, guests = [], []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit():
            continue
        try:
            group = (proc / 'cgroup').read_text()
            exe = os.readlink(proc / 'exe')
            comm = (proc / 'comm').read_text().strip()
            binary = Path(exe.removesuffix(' (deleted)')).name
            if 'shim-cube' in binary or binary == 'cube-runtime':
                guests.append(int(proc.name))
            if owned_cgroup(group, units):
                owned.append(
                    {'pid': int(proc.name), 'start': (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]}
                )
            elif exe.startswith(('/opt/brocade/cvm/', '/data/cubelet/')) or comm == 'cubelet':
                guests.append(int(proc.name))
        except (FileNotFoundError, ProcessLookupError):
            continue
    return owned, guests


def stop_services(runner, units):
    present = [
        unit
        for unit in units
        if runner.text('systemctl', 'show', unit, '-p', 'LoadState', '--value').strip() != 'not-found'
    ]
    if present:
        runner.change('systemctl', 'disable', '--now', *present)
    owned, _ = process_inventory(units)
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for process in owned:
            proc = Path('/proc', str(process['pid']))
            try:
                if (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19] == process['start']:
                    os.kill(process['pid'], sig)
            except (FileNotFoundError, ProcessLookupError):
                pass
        until = time.monotonic() + 5
        while time.monotonic() < until:
            owned, _ = process_inventory(units)
            if not owned:
                return
            time.sleep(0.1)
    raise CleanupError('Brocade service processes did not exit')


def cube_bpf(runner, apply):
    root = Path('/sys/fs/bpf')
    pins = sorted(path for path in root.iterdir() if path.name in CUBE_PINS) if root.exists() else []
    if not pins:
        return []
    if not shutil.which('bpftool'):
        raise CleanupError('Cube BPF pins exist; bpftool is required')
    programs = []
    for name in ('from_cube', 'from_world', 'from_envoy'):
        if (root / name).exists():
            programs.append(runner.json('bpftool', '-j', 'prog', 'show', 'pinned', str(root / name)))
    ids = {p['id'] for p in programs}
    if apply:
        for link in runner.json('ip', '-j', 'link', 'show'):
            for direction in ('ingress', 'egress'):
                for rule in runner.json('tc', '-j', 'filter', 'show', 'dev', link['ifname'], direction):
                    options = rule.get('options', {})
                    program = options.get('prog', {})
                    if program.get('id') in ids or program.get('name') in CUBE_PROGRAMS:
                        runner.change(
                            'tc',
                            'filter',
                            'delete',
                            'dev',
                            link['ifname'],
                            direction,
                            'protocol',
                            rule['protocol'],
                            'pref',
                            str(rule['pref']),
                            'chain',
                            str(rule.get('chain', 0)),
                            'handle',
                            str(options['handle']),
                            'bpf',
                        )
        for path in pins:
            if path.is_symlink() or path.is_dir():
                raise CleanupError('Unexpected Cube BPF pin type')
            path.unlink()
            runner.actions.append({'time': timestamp(), 'unpinned': str(path)})
    return [str(path) for path in pins]


def cleanup_paths(args):
    paths = [args.bin_dir / 'brocade-agent', Path('/etc/sysctl.d/99-brocade.conf')]
    paths += [Path('/etc/systemd/system') / unit for unit in (UNITS if args.cube else UNITS[:2])]
    paths += [Path('/etc/systemd/system/brocade-agent.service.d'), Path('/etc/systemd/journald@brocade-agent.conf.d')]
    paths += [
        Path('/etc/modules-load.d') / name
        for name in ('brocade.conf', 'brocade-xray-ktls.conf', 'brocade-vpngate.conf')
    ]
    if args.remove_xray:
        paths.append(args.bin_dir / 'xray')
    if args.purge_data:
        paths += [
            args.state_dir,
            args.config_dir,
            Path('/etc/brocade/cvm-host.json'),
            Path('/var/lib/brocade/cvm-host'),
        ]
        for root in (Path('/var/log/journal'), Path('/run/log/journal')):
            if root.is_dir():
                paths.extend(p for p in root.iterdir() if re.fullmatch(r'[0-9a-f]{32}\.brocade-agent', p.name))
        paths += [Path('/etc/netns') / name for name in args.namespace]
    if args.cube:
        paths += [
            Path('/etc/modules-load.d/brocade-cvm.conf'),
            Path('/usr/local/lib/brocade/cvm-stack.py'),
            Path('/opt/brocade/cvm'),
        ]
        if args.purge_data:
            paths += [Path('/data/cubelet'), Path('/var/lib/brocade/cvm-cube')]
    return [safe_path(path) for path in paths]


def reject_shared_binaries(paths, owned):
    selected = {str(p) for p in paths if p.is_file()}
    if not selected:
        return
    owned_pids = {p['pid'] for p in owned}
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) in owned_pids:
            continue
        try:
            if os.readlink(proc / 'exe').removesuffix(' (deleted)') in selected:
                raise CleanupError(f'Binary in use outside Brocade service cgroups (PID {proc.name})')
        except (FileNotFoundError, ProcessLookupError):
            pass


def unmount_cube(runner):
    rows = runner.json('findmnt', '-J', '-l', '-o', 'TARGET,SOURCE,FSTYPE')['filesystems']
    for mount in sorted(rows, key=lambda row: len(row['target']), reverse=True):
        target = mount['target']
        if target == '/data/cubelet/state' or re.fullmatch(
            r'/opt/brocade/cvm/releases/[0-9a-f]{64}/runtime/cubeletmnt(?:/mnt)?', target
        ):
            if mount['fstype'] != 'nsfs' and not mount['source'].endswith(f'[{target}]'):
                raise CleanupError(f'Unexpected filesystem at Cube mount {target}; retained')
            runner.change('umount', '--', target)


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument('--apply', action='store_true', help='execute the printed cleanup; default is read-only')
    result.add_argument(
        '--purge-data',
        action='store_true',
        help='also permanently delete Agent credentials/state and selected CVM data',
    )
    result.add_argument(
        '--cube',
        action='store_true',
        help='also remove the managed Cube runtime and CubeVS BPF pins; guests must be stopped',
    )
    result.add_argument('--remove-xray', action='store_true', help='also delete the installed Xray binary')
    result.add_argument(
        '--interface',
        action='append',
        default=[],
        help='explicitly claim an otherwise unlabelled interface (repeatable)',
    )
    result.add_argument(
        '--namespace',
        action='append',
        default=[],
        help='explicitly claim an otherwise unlabelled namespace (repeatable)',
    )
    result.add_argument('--cube-router', default='cube-router', help='exact interface name in legacy Cube NAT rules')
    result.add_argument(
        '--nat-source',
        action='append',
        default=[],
        type=ipaddress.ip_network,
        help='explicitly claim a former VPN Gate SNAT source CIDR (repeatable)',
    )
    result.add_argument(
        '--cube-nat-ip',
        action='append',
        default=[],
        type=ipaddress.IPv4Address,
        help='explicitly claim a former Cube NAT IPv4 address (repeatable)',
    )
    result.add_argument('--state-dir', type=Path, default=Path('/var/lib/brocade-agent'))
    result.add_argument('--config-dir', type=Path, default=Path('/etc/brocade-agent'))
    result.add_argument('--bin-dir', type=Path, default=Path('/usr/local/bin'))
    result.add_argument('--report', type=Path, help='write a new private JSON report; never overwrite an existing file')
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    if os.geteuid() != 0:
        raise CleanupError('Run with sudo, including the read-only preview, to inspect firewall ownership')
    for name in (*args.interface, *args.namespace, args.cube_router):
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}', name):
            raise CleanupError('Invalid interface or namespace name')
    if any(net.prefixlen < (24 if net.version == 4 else 64) for net in args.nat_source):
        raise CleanupError('SNAT source claims must be at least IPv4 /24 or IPv6 /64')
    validate_directories(args)
    started = time.monotonic()
    report = {'started': timestamp(), 'apply': args.apply, 'result': 'failed'}
    runner = Runner()
    fd = None
    lock = None
    done = threading.Event()

    def progress():
        while not done.wait(30):
            print(f'cleanup-agent: running, elapsed {time.monotonic() - started:.1f}s', flush=True)

    watcher = threading.Thread(target=progress, daemon=True)
    watcher.start()
    try:
        paths = cleanup_paths(args)
        if args.report and any(args.report.absolute() == p or p in args.report.absolute().parents for p in paths):
            raise CleanupError('Report path is inside a selected cleanup path')
        if args.report:
            fd = os.open(args.report, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
        owned, guests = process_inventory()
        if guests:
            raise CleanupError('Cube/Guest processes exist outside Agent service cgroups; shut down instances first')
        network = Network(runner, args)
        plan = network.inventory()
        reject_shared_binaries(paths, owned)
        pins = cube_bpf(runner, False) if args.cube else []
        report['plan'] = plan | {'paths': [str(p) for p in paths], 'cube_bpf_pins': pins}
        public_plan = report['plan'] | {
            'firewalls': [{'tool': fw['tool'], 'owned': fw['owned']} for fw in plan['firewalls']]
        }
        print(json.dumps(public_plan, ensure_ascii=False, indent=2), flush=True)
        if plan['unclaimed_namespaces']:
            raise CleanupError('Unverified namespaces remain; inspect before explicitly selecting --namespace')
        if plan.get('conflicting_tables'):
            raise CleanupError('A named nft table has a conflicting ownership comment; retained')
        if args.apply:
            lock = os.open('/run/lock/brocade-agent-cleanup.lock', os.O_CREAT | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            stop_services(runner, UNITS if args.cube else UNITS[:2])
            plan = network.inventory()  # Stop convergence before freezing the actual network cleanup.
            report['executed_network_plan'] = plan
            if plan['unclaimed_namespaces']:
                raise CleanupError('Unverified namespaces appeared during service shutdown')
            if args.cube:
                cube_bpf(runner, True)
            network.clear(plan)
            if args.cube:
                unmount_cube(runner)
            for suffix in (
                'systemd-journald-varlink@brocade-agent.socket',
                'systemd-journald@brocade-agent.socket',
                'systemd-journald@brocade-agent.service',
            ):
                if runner.text('systemctl', 'show', suffix, '-p', 'LoadState', '--value').strip() != 'not-found':
                    runner.change('systemctl', 'stop', suffix)
            mounts = runner.text('findmnt', '-rn', '-o', 'TARGET').splitlines()
            for path in paths:
                safe_path(path)
                if not path.exists():
                    continue
                if any(m == str(path) or m.startswith(str(path) + '/') for m in mounts):
                    raise CleanupError(f'Mounted data under {path}; retained')
                if path.is_dir():
                    shutil.rmtree(path)
                else:
                    path.unlink()
                runner.actions.append({'time': timestamp(), 'removed': str(path)})
            runner.change('systemctl', 'daemon-reload')
            if process_inventory(UNITS if args.cube else UNITS[:2])[0]:
                raise CleanupError('Brocade service processes remain after cleanup')
        report['result'] = 'success' if args.apply else 'preview'
        return 0
    except (CleanupError, OSError) as error:
        report['error'] = str(error)
        print(f'cleanup-agent: {error}', file=sys.stderr)
        return 1
    finally:
        done.set()
        watcher.join()
        if lock is not None:
            os.close(lock)
        report.update(
            finished=timestamp(), elapsed_seconds=round(time.monotonic() - started, 3), actions=runner.actions
        )
        if fd is not None:
            with os.fdopen(fd, 'w') as stream:
                json.dump(report, stream, ensure_ascii=False, indent=2)
                stream.write('\n')
        print(
            f'{report["result"]}: {report["started"]} -> {report["finished"]}, {report["elapsed_seconds"]}s', flush=True
        )


if __name__ == '__main__':
    try:
        sys.exit(main())
    except CleanupError as error:
        sys.exit(str(error))
