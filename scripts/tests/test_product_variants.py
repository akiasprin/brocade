"""Execute the shipped installers' identity gates without touching host services."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def executable(path, body):
    path.write_text('#!/bin/sh\n' + body + '\n')
    path.chmod(0o700)


class ProductInstallerTests(unittest.TestCase):
    def test_console_manifest_candidate_and_explicit_switch_must_agree(self):
        source = (ROOT / 'install.sh').read_text()
        gate = source[source.index('IFS= read -r bundle_flavor'):source.index('case $(uname -m) in')]
        for bundle, candidate, installed, explicit, accepted in [
            ('base', 'base', None, '', True),
            ('cvm', 'cvm', None, '', True),
            ('base', 'cvm', None, '', False),
            ('base', 'base', 'cvm', '', False),
            ('base', 'base', 'cvm', 'base', True),
            ('cvm', 'cvm', 'base', 'cvm', True),
            ('base', 'base', 'base', '', True),
            ('base', 'base', 'unknown', 'base', False),
        ]:
            with self.subTest(bundle=bundle, installed=installed, explicit=explicit), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'BROCADE_FLAVOR').write_text(bundle + '\n')
                executable(root / 'brocade-console', 'printf \'{"flavor":"' + candidate + '","features":{"cvm":false}}\\n\'')
                if installed is not None:
                    (root / 'installed').write_text(installed + '\n')
                result = subprocess.run(['sh', '-eu', '-c',
                    'die() { exit 7; }; initialize_product=false; script_dir=$1; product_flavor_path=$1/installed; requested_flavor=$2; upgrade=$3;\n' + gate,
                    'gate', directory, explicit, str(installed is not None).lower()], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)

    def test_console_first_identity_is_explicit_and_base_only(self):
        source = (ROOT / 'install.sh').read_text()
        gate = source[source.index('IFS= read -r bundle_flavor'):source.index('case $(uname -m) in')]
        for bundle, installed, explicit, initialize, accepted in [
            ('base', None, '', False, False),
            ('base', None, 'base', False, False),
            ('base', None, 'base', True, True),
            ('cvm', None, 'cvm', True, False),
            ('base', 'base', 'base', True, False),
            ('base', 'unknown', 'base', True, False),
        ]:
            with self.subTest(bundle=bundle, installed=installed, initialize=initialize), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'BROCADE_FLAVOR').write_text(bundle + '\n')
                executable(root / 'brocade-console', 'printf \'{"flavor":"' + bundle + '"}\\n\'')
                if installed is not None:
                    (root / 'installed').write_text(installed + '\n')
                result = subprocess.run(['sh', '-eu', '-c',
                    'die() { exit 7; }; script_dir=$1; product_flavor_path=$1/installed; requested_flavor=$2; initialize_product=$3; upgrade=true;\n' + gate,
                    'gate', directory, explicit, str(initialize).lower()], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)
                self.assertEqual((root / 'installed').exists(), installed is not None)

    def test_network_only_cvm_installs_host_tools_without_compute_stack(self):
        source = (ROOT / 'crates/brocade-console/templates/install.sh').read_text()
        gate = source[source.index('# CVM network providers'):source.index('\ninstall -d -m 0755 "$INSTALL_DIR"')]
        for flavor, manager, expected_tc in [('base', '', None), ('cvm', '', 'iproute2'), ('cvm', 'apk', 'iproute2-tc'), ('cvm', 'dnf', 'iproute-tc')]:
            with self.subTest(flavor=flavor, manager=manager), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'available').mkdir()
                setup = '''
have() { [ "$1" = "$MANAGER" ] || [ -f "$AVAILABLE/$1" ]; }
install_pkg() {
    printf '%s\\n' "$1" >> "$PACKAGES"
    case $1 in
        iproute|iproute2) touch "$AVAILABLE/ip" "$AVAILABLE/ss"; [ "$MANAGER" != '' ] || touch "$AVAILABLE/tc" ;;
        iproute2-tc|iproute-tc) touch "$AVAILABLE/tc" ;;
        nftables) touch "$AVAILABLE/nft" ;;
        conntrack) touch "$AVAILABLE/conntrack" ;;
        wireguard-tools) touch "$AVAILABLE/wg" ;;
        util-linux) touch "$AVAILABLE/findmnt" "$AVAILABLE/nsenter" "$AVAILABLE/mount" "$AVAILABLE/umount" ;;
        *) exit 9 ;;
    esac
    return 0
}
'''
                env = {**os.environ, 'AGENT_FLAVOR': flavor, 'APPLY_MODE': 'linux', 'ENABLE_CVM': '0',
                       'MANAGER': manager, 'AVAILABLE': str(root / 'available'), 'PACKAGES': str(root / 'packages')}
                result = subprocess.run(['sh', '-eu', '-c', setup + gate], env=env, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                packages = (root / 'packages').read_text().splitlines() if (root / 'packages').exists() else []
                if expected_tc is None:
                    self.assertEqual(packages, [])
                else:
                    self.assertIn(expected_tc, packages)
                    for tool in ['ip', 'tc', 'ss', 'nft', 'conntrack', 'wg', 'findmnt', 'nsenter', 'mount', 'umount']:
                        self.assertTrue((root / 'available' / tool).exists(), tool)

    def test_node_install_preserves_identity_and_rejects_before_enrollment(self):
        source = (ROOT / 'crates/brocade-console/templates/install.sh').read_text()
        gate = source[source.index('# Product identity is required'):source.index('\nverify_agent_candidate()')]
        for installed, explicit, supported, compute, accepted, expected in [
            (None, '', '0', '0', True, 'base'),
            (None, '', '1', '1', True, 'cvm'),
            ('cvm', '', '1', '0', True, 'cvm'),
            ('cvm', '', '0', '0', False, None),
            ('base', 'cvm', '0', '0', False, None),
            ('base', 'base', '1', '1', False, None),
            ('unknown', 'base', '1', '0', False, None),
            ('cvm', 'base', '1', '0', True, 'base'),
        ]:
            with self.subTest(installed=installed, explicit=explicit), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'token').write_text('test-only-token')
                if installed is not None:
                    executable(root / 'brocade-agent',
                        'case $1 in self-test) printf "brocade-agent-self-test-v1\\nbrocade-agent-product:' + installed + ':v1\\n";; product-check) touch "$BROCADE_CHECK_MARKER";; *) exit 2;; esac')
                env = {**os.environ, 'INSTALL_DIR': directory, 'STATE_DIR': directory,
                       'CONFIG_DIR': directory, 'SERVER': 'https://console.example',
                       'AGENT_FLAVOR': explicit, 'AGENT_FLAVOR_EXPLICIT': explicit,
                       'CVM_SUPPORTED': supported, 'ENABLE_CVM': compute,
                       'BROCADE_CHECK_MARKER': str(root / 'checked')}
                result = subprocess.run(['sh', '-eu', '-c', 'have() { return 1; };\n' + gate + '\nprintf "%s" "$AGENT_FLAVOR"'],
                                        env=env, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)
                if accepted:
                    self.assertEqual(result.stdout, expected)
                self.assertEqual((root / 'checked').exists(), installed == 'cvm' and expected == 'base')

    def test_candidate_wrong_identity_or_old_probe_is_rejected(self):
        source = (ROOT / 'crates/brocade-console/templates/install.sh').read_text()
        start = source.index('verify_agent_candidate()')
        end = source.index('\n}\n', start) + 3
        for output, accepted in [
            ('brocade-agent-self-test-v1\\nbrocade-agent-product:base:v1', True),
            ('brocade-agent-self-test-v1\\nbrocade-agent-product:cvm:v1', False),
            ('brocade-agent-self-test-v1', False),
            ('usage: brocade-agent', False),
        ]:
            with self.subTest(output=output), tempfile.TemporaryDirectory() as directory:
                candidate = Path(directory) / 'candidate'
                executable(candidate, 'printf "' + output + '\\n"')
                result = subprocess.run(['sh', '-eu', '-c', 'AGENT_FLAVOR=base;\n' + source[start:end] + '\nverify_agent_candidate "$1"',
                                         'check', str(candidate)], capture_output=True, timeout=5)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)


if __name__ == '__main__':
    unittest.main()
