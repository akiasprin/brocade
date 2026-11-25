"""Agent uninstall boundaries; real firewall checks run only in an isolated network namespace."""

import contextlib
import importlib.util
import io
import ipaddress
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/cleanup-agent.py'
SPEC = importlib.util.spec_from_file_location('cleanup_agent', SCRIPT)
cleanup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cleanup)


class OwnershipTests(unittest.TestCase):
    def test_cube_pin_inventory_covers_current_lan_and_quota_maps(self):
        source = (ROOT / 'components/cube/CubeNet/src/quota.bpf.c').read_text()
        quota = set(re.findall(r'^QMAP\((cube_\w+),', source, re.MULTILINE))
        source = (ROOT / 'components/cube/CubeNet/src/lan.bpf.c').read_text()
        lan = set(re.findall(r'^\} (cube_lan_\w+) SEC', source, re.MULTILINE))
        self.assertEqual(len(quota), 5)
        self.assertEqual(len(lan), 4)
        self.assertTrue((quota | lan).issubset(cleanup.CUBE_PINS))

    def test_table_selection_includes_nat_without_matching_arbitrary_prefixes(self):
        for family, name in [
            ('ip', 'brocade_cvm_entries'),
            ('inet', 'brocade_hy2_nat'),
            ('inet', 'brocade_entry_phantun'),
        ]:
            self.assertTrue(cleanup.owned_table({'family': family, 'name': name}))
        for name in ('nat', 'filter', 'DOCKER', 'brocade_custom', 'brocade_hy2_nat_backup'):
            self.assertFalse(cleanup.owned_table({'family': 'ip', 'name': name}))
        table = {'family': 'bridge', 'name': 'bcg012345abcdef', 'comment': 'other-owner'}
        self.assertFalse(cleanup.owned_table(table))
        table['comment'] = 'brocade-nic:nic-example'
        self.assertTrue(cleanup.owned_table(table))

    def test_cube_rules_require_exact_nat_identity_and_full_shape(self):
        source = '198.18.64.2'
        rules = [
            (
                'nat',
                f'-A POSTROUTING -s {source}/32 ! -o cube-router -p tcp -m tcp -j MASQUERADE --to-ports 32768-65535',
            ),
            ('nat', f'-A POSTROUTING -s {source}/32 ! -o cube-router -p udp -j MASQUERADE'),
            ('nat', f'-A POSTROUTING -s {source}/32 ! -o cube-router -p icmp -j MASQUERADE'),
            ('filter', f'-A FORWARD -i cube-router -s {source}/32 -j ACCEPT'),
            (
                'filter',
                f'-A FORWARD -o cube-router -d {source}/32 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT',
            ),
        ]
        for table, line in rules:
            args = shlex.split(line)
            self.assertTrue(cleanup.owned_rule(table, args, 'cube-router', [ipaddress.IPv4Address(source)]))
            self.assertFalse(cleanup.owned_rule(table, args, 'cube-router', []))
            self.assertFalse(cleanup.owned_rule(table, args, 'cube-router', ['198.18.64.3']))
            self.assertFalse(cleanup.owned_rule(table, args + ['--random-fully'], 'cube-router', [source]))
        docker = shlex.split('-A POSTROUTING -s 172.18.0.0/16 ! -o docker0 -j MASQUERADE')
        self.assertFalse(cleanup.owned_rule('nat', docker, 'cube-router', [source]))

    def test_comment_matching_is_token_exact(self):
        args = shlex.split('-A DOCKER-USER -i bep+ -m comment --comment brocade-entry-phantun -j ACCEPT')
        self.assertTrue(cleanup.owned_rule('filter', args, 'cube-router', []))
        args[args.index('--comment') + 1] += '-unrelated'
        self.assertFalse(cleanup.owned_rule('filter', args, 'cube-router', []))

    def test_vpngate_rules_need_explicit_claims_including_ipv6(self):
        for source in ('169.254.128.2/32', '2001:db8::2/128'):
            rule = shlex.split(f'-A POSTROUTING -s {source} -j MASQUERADE')
            self.assertFalse(cleanup.owned_rule('nat', rule, 'cube-router', []))
            self.assertTrue(cleanup.owned_rule('nat', rule, 'cube-router', [], [ipaddress.ip_network(source)]))
            self.assertFalse(cleanup.owned_rule('nat', rule + ['--random-fully'], 'cube-router', [], [source]))
        rule = shlex.split('-A FORWARD -o bvg0001h -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT')
        self.assertFalse(cleanup.owned_rule('filter', rule, 'cube-router', []))
        self.assertTrue(cleanup.owned_rule('filter', rule, 'cube-router', [], interfaces=['bvg0001h']))

    def test_conflicting_owner_or_namespace_process_prevents_any_network_mutation(self):
        runner = Mock()
        network = cleanup.Network(runner, cleanup.parser().parse_args([]))
        table = {'family': 'inet', 'name': 'brocade_host_output', 'comment': 'foreign-owner'}
        self.assertFalse(cleanup.owned_table(table))
        with self.assertRaises(cleanup.CleanupError):
            network.clear({'conflicting_tables': [table]})
        runner.change.assert_not_called()
        runner.text.return_value = '12345\n'
        with self.assertRaises(cleanup.CleanupError):
            network.clear({'namespaces': [{'name': 'bcn-test', 'inode': 1}]})
        runner.change.assert_not_called()

    def test_both_address_families_and_table_boundaries_are_parsed(self):
        source = '*nat\n-A POSTROUTING -j ACCEPT\nCOMMIT\n*filter\n-A INPUT -j ACCEPT\nCOMMIT\n'
        self.assertEqual([row[0] for row in cleanup.parse_iptables(source)], ['nat', 'filter'])
        self.assertEqual(cleanup.number('12058624'), 0xB80000)
        self.assertFalse(cleanup.policy_table(254))
        self.assertTrue(cleanup.policy_table(0xB80000))

    def test_cgroup_identity_handles_newlines_and_subgroups_without_prefix_matching(self):
        self.assertTrue(cleanup.owned_cgroup('0::/system.slice/brocade-agent.service\n', cleanup.UNITS))
        self.assertTrue(cleanup.owned_cgroup('0::/system.slice/brocade-agent.service/child\n', cleanup.UNITS))
        self.assertFalse(cleanup.owned_cgroup('0::/system.slice/brocade-agent.service-other\n', cleanup.UNITS))

    def test_dangerous_custom_paths_and_symlinks_are_rejected(self):
        for path in ('/', '/etc', '/home', '/home/example', '/var/lib/postgresql', '/var/lib/brocade'):
            args = cleanup.parser().parse_args(['--state-dir', path])
            with self.assertRaises(cleanup.CleanupError):
                cleanup.validate_directories(args)
        with tempfile.TemporaryDirectory() as root:
            parent = Path(root)
            (parent / 'real').mkdir()
            (parent / 'brocade-link').symlink_to(parent / 'real')
            with self.assertRaises(cleanup.CleanupError):
                cleanup.safe_path(parent / 'brocade-link')

    def test_preview_never_stops_services_changes_network_or_removes_files(self):
        plan = {
            'tables': [],
            'firewalls': [],
            'links': [],
            'namespaces': [],
            'unclaimed_namespaces': [],
            'route_tables': [],
        }
        with (
            patch.object(cleanup.os, 'geteuid', return_value=0),
            patch.object(cleanup, 'process_inventory', return_value=([], [])),
            patch.object(cleanup, 'cleanup_paths', return_value=[]),
            patch.object(cleanup.Network, 'inventory', return_value=plan),
            patch.object(cleanup.Network, 'clear') as clear,
            patch.object(cleanup, 'stop_services') as stop,
            patch.object(cleanup.shutil, 'rmtree') as remove,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(cleanup.main([]), 0)
            clear.assert_not_called()
            stop.assert_not_called()
            remove.assert_not_called()

    def test_default_paths_keep_cube_runtime_and_data(self):
        args = cleanup.parser().parse_args([])
        paths = cleanup.cleanup_paths(args)
        self.assertNotIn(Path('/etc/systemd/system/brocade-cvm-cubelet.service'), paths)
        self.assertNotIn(Path('/var/lib/brocade-agent'), paths)
        self.assertNotIn(Path('/usr/local/bin/xray'), paths)

    def test_non_self_bind_cube_mount_is_retained(self):
        runner = Mock()
        runner.json.return_value = {
            'filesystems': [{'target': '/data/cubelet/state', 'source': '/dev/test', 'fstype': 'ext4'}]
        }
        with self.assertRaises(cleanup.CleanupError):
            cleanup.unmount_cube(runner)
        runner.change.assert_not_called()


@unittest.skipUnless(os.environ.get('BROCADE_RUN_CLEANUP_NETNS_TESTS') == '1', 'requires explicit private netns test')
class NetnsTests(unittest.TestCase):
    def test_real_ipv4_ipv6_nat_cleanup_preserves_other_tables_and_is_repeatable(self):
        for tool in ('unshare', 'nft', 'ip', 'iptables', 'ip6tables'):
            self.assertIsNotNone(shutil.which(tool), tool)
        # The child imports only Network: it never stops host systemd services or removes host files.
        program = r"""
import importlib.util, sys
from pathlib import Path
spec=importlib.util.spec_from_file_location('cleanup_agent',sys.argv[1])
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
r=m.Runner()
for family in ('ip','ip6'):
    r.change('nft','add','table',family,'unrelated_nat')
    r.change('nft','add','chain',family,'unrelated_nat','keep','{','type','nat','hook','postrouting','priority','99',';','}')
    r.change('nft','add','rule',family,'unrelated_nat','keep','counter','masquerade')
r.change('nft','add','table','inet','brocade_hy2_nat')
r.change('nft','add','table','ip','brocade_cvm_entries')
r.change('ip','link','add','btest','type','dummy')
r.change('ip','link','set','btest','alias','brocade-entry:test')
for tool in ('iptables','ip6tables'):
    r.change(tool,'-t','filter','-N','DOCKER-USER')
    r.change(tool,'-t','filter','-A','DOCKER-USER','-i','bep+','-m','comment','--comment','brocade-entry-phantun','-j','ACCEPT')
    r.change(tool,'-t','filter','-A','DOCKER-USER','-s','127.0.0.1/32' if tool=='iptables' else '::1/128','-j','ACCEPT')
r.change('iptables','-t','nat','-A','POSTROUTING','-s','198.18.64.2/32','!','-o','cube-router','-p','udp','-j','MASQUERADE')
r.change('iptables','-t','nat','-A','POSTROUTING','-s','172.18.0.0/16','!','-o','docker0','-j','MASQUERADE')
r.change('ip6tables','-t','nat','-A','POSTROUTING','-s','2001:db8::2/128','-j','MASQUERADE')
args=m.parser().parse_args(['--cube-nat-ip','198.18.64.2','--nat-source','2001:db8::2/128'])
n=m.Network(r,args);p=n.inventory()
assert len(p['tables'])==2 and len(p['links'])==1
assert sum(len(fw['owned']) for fw in p['firewalls'])>=4
n.clear(p)
n.clear(n.inventory())
for family in ('ip','ip6'):
    assert 'masquerade' in r.text('nft','list','table',family,'unrelated_nat')
assert '172.18.0.0/16' in r.text('iptables-save')
assert '127.0.0.1/32' in r.text('iptables-save')
assert '::1/128' in r.text('ip6tables-save')
assert '198.18.64.2' not in r.text('iptables-save')
assert '2001:db8::2/128' not in r.text('ip6tables-save')
print('private-netns-cleanup-passed')
"""
        result = subprocess.run(
            ['unshare', '--user', '--map-root-user', '--net', sys.executable, '-c', program, str(SCRIPT)],
            check=False,
            capture_output=True,
            text=True,
            timeout=90,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('private-netns-cleanup-passed', result.stdout)


if __name__ == '__main__':
    unittest.main()
