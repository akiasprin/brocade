"""Prove that a channel upgrade cannot bypass an HTTP receipt fault."""

import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from release_kvm_lab import Lab, free_port
from release_proxy import MAX_BODY, STACK_POLL
from release_stack_acceptance import StackAcceptance, cvm_stack, empty_observation, make_catalog, openssl, sign_pair


class LabRouteValidationTests(unittest.TestCase):
    def test_public_runtime_is_rejected_before_qmp_or_ssh(self):
        with tempfile.TemporaryDirectory() as directory:
            lab = object.__new__(Lab)
            lab.output = Path(directory)
            runtime = lab.output / 'runtime'
            runtime.mkdir()
            runtime.chmod(0o755)
            lab.state = {'runtime': str(runtime)}
            with patch.object(lab, 'qmp') as qmp, patch.object(lab, 'remote') as remote:
                with self.assertRaisesRegex(RuntimeError, 'private and owned'):
                    lab.verify_owned()
                qmp.assert_not_called()
                remote.assert_not_called()

    def test_invalid_routes_are_rejected_before_remote_mutation(self):
        lab = object.__new__(Lab)
        lab.state = {
            'roles': {
                'compute': {'ipv4': '172.30.1.10', 'ipv6': 'fdc0:1234:1::1'},
                'client': {'ipv4': '172.30.1.30', 'ipv6': 'fdc0:1234:1::3'},
            }
        }
        for role, routes in [
            ('unknown', [('11.254.0.0/24', '172.30.1.30')]),
            ('compute', [('0.0.0.0/0', '172.30.1.30')]),
            ('compute', [('11.254.0.0/24', '203.0.113.1')]),
            ('compute', [('11.254.0.0/24', 'fdc0:1234:1::3')]),
            ('compute', [('11.254.0.0/24', '172.30.1.10')]),
        ]:
            with self.subTest(role=role, routes=routes), self.assertRaises(ValueError):
                lab.persist_routes(role, routes)


class ReceiptFaultTests(unittest.TestCase):
    def test_channel_upgrade_is_rejected_and_actual_receipt_is_captured(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            authorization = 'Bearer owned-acceptance-test-token'
            control = {
                'nodes': {hashlib.sha256(authorization.encode()).hexdigest(): 'compute-a'},
                'hold_machine': ['compute-a'],
            }
            (root / 'control.json').write_text(json.dumps(control))
            with socket.socket() as listener:
                listener.bind(('127.0.0.1', 0))
                port = listener.getsockname()[1]
            process = subprocess.Popen(
                [
                    sys.executable,
                    str(Path(__file__).with_name('release_proxy.py')),
                    '--root',
                    str(root),
                    '--port',
                    str(port),
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                deadline = time.monotonic() + 5
                while True:
                    try:
                        with socket.create_connection(('127.0.0.1', port), timeout=0.2):
                            break
                    except OSError:
                        if process.poll() is not None or time.monotonic() >= deadline:
                            self.fail('owned receipt proxy did not start')
                        time.sleep(0.02)
                connection = http.client.HTTPConnection('127.0.0.1', port, timeout=2)
                connection.request(
                    'GET', '/agent/v1/channel', headers={'Upgrade': 'websocket', 'Connection': 'Upgrade'}
                )
                response = connection.getresponse()
                self.assertEqual(response.status, 503)
                response.read()
                connection.close()
                receipt = b'{"deployment_id":17,"status":"succeeded","actual_runtime":true}'
                connection = http.client.HTTPConnection('127.0.0.1', port, timeout=2)
                connection.request(
                    'POST', '/agent/v1/observation', body=receipt, headers={'Authorization': authorization}
                )
                response = connection.getresponse()
                self.assertEqual(response.status, 503)
                response.read()
                connection.close()
                self.assertEqual((root / 'compute-a-machine.json').read_bytes(), receipt)
                summary = json.loads((root / 'compute-a-machine.summary.json').read_text())
                self.assertEqual(summary['sha256'], hashlib.sha256(receipt).hexdigest())
            finally:
                process.terminate()
                process.wait(timeout=5)


class StackFaultTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bundle = b'actual upstream bundle\n' * (MAX_BODY // 23 + 1024)
        self.sha = hashlib.sha256(self.bundle).hexdigest()
        self.route = '/agent/v1/cvm/stack-bundles/' + self.sha
        self.received = []
        fixture = self

        class Upstream(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.send_response(200)
                self.send_header('Content-Length', str(len(fixture.bundle)))
                self.end_headers()
                try:
                    self.wfile.write(fixture.bundle)
                except OSError:
                    pass

            def do_POST(self):
                fixture.received.append(self.rfile.read(int(self.headers['Content-Length'])))
                self.send_response(200)
                self.send_header('Content-Length', '0')
                self.end_headers()

        server = ThreadingHTTPServer(('127.0.0.1', 0), Upstream)
        self.addCleanup(server.server_close)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        self.port = free_port()
        self.auth = 'Bearer owned-lab-fake-token'
        self.configure()
        proxy = subprocess.Popen(
            [
                sys.executable,
                str(Path(__file__).with_name('release_proxy.py')),
                '--root',
                str(self.root),
                '--port',
                str(self.port),
                '--upstream-port',
                str(server.server_port),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        def stop():
            proxy.terminate()
            proxy.wait(timeout=5)

        self.addCleanup(stop)
        deadline = time.monotonic() + 5
        while True:
            try:
                with socket.create_connection(('127.0.0.1', self.port), timeout=0.2):
                    break
            except OSError:
                if proxy.poll() is not None or time.monotonic() >= deadline:
                    self.fail('owned stack proxy did not start')
                time.sleep(0.02)

    def configure(self, **faults):
        (self.root / 'control.json').write_text(
            json.dumps(
                {
                    'nodes': {hashlib.sha256(self.auth.encode()).hexdigest(): 'compute-a'},
                    **faults,
                }
            )
        )

    def request(self, method, path, body=None, auth=True):
        connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=5)
        try:
            connection.request(method, path, body=body, headers={'Authorization': self.auth} if auth else {})
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()

    def test_streams_large_bundle_and_faults_only_matching_node_and_digest(self):
        self.assertGreater(len(self.bundle), MAX_BODY)
        self.assertEqual(self.request('GET', self.route), (200, self.bundle))
        for mode in ('hash', 'size'):
            self.configure(stack_download={'node': 'compute-a', 'sha256': self.sha, 'mode': mode})
            status, contents = self.request('GET', self.route)
            self.assertEqual(status, 200)
            if mode == 'hash':
                self.assertEqual(len(contents), len(self.bundle))
                self.assertNotEqual(hashlib.sha256(contents).hexdigest(), self.sha)
            else:
                self.assertEqual(contents, self.bundle[:-1])
            proof = json.loads((self.root / 'compute-a-stack-download.summary.json').read_text())
            self.assertEqual(proof, {'mode': mode, 'path': self.route, 'upstream_bytes': len(self.bundle)})
            self.assertEqual(self.request('GET', self.route, auth=False), (200, self.bundle))
            self.assertEqual(self.request('GET', '/agent/v1/cvm/stack-bundles/' + 'a' * 64), (200, self.bundle))

    def test_holds_real_claim_only_on_the_configured_node_and_route(self):
        self.configure(hold_stack_claim=['compute-a'])
        route = '/agent/v1/cvm/stack-releases/17/claim'
        roots = 'd' * 64
        claim = json.dumps({'roots_sha256': roots}).encode()
        wrong_route = route + '/extra'
        invalid_claim = b'{"roots_sha256":"invalid"}'
        self.assertEqual(self.request('POST', route, claim, auth=False)[0], 200)
        self.assertEqual(self.request('POST', wrong_route, claim)[0], 200)
        self.assertEqual(self.request('POST', STACK_POLL, claim)[0], 200)
        self.assertEqual(self.request('POST', route, invalid_claim)[0], 200)
        self.assertEqual(self.request('POST', route, claim)[0], 503)
        self.assertEqual(self.received, [claim, claim, claim, invalid_claim])
        evidence = json.loads((self.root / 'compute-a-stack-claim.summary.json').read_text())
        self.assertEqual(evidence, {'id': 17, 'path': route, 'roots_sha256': roots})
        self.configure()
        self.assertEqual(self.request('POST', route, claim)[0], 200)
        self.assertEqual(self.received, [claim, claim, claim, invalid_claim, claim])

    def test_holds_only_matching_terminal_receipt_then_forwards_original_request(self):
        self.configure(hold_stack_report={'node': 'compute-a', 'id': 17, 'status': 'succeeded'})
        report = b'{"report":{"id":17,"status":"succeeded","roots_sha256":"proof"}}'
        other = report.replace(b'17', b'18')
        self.assertEqual(self.request('POST', STACK_POLL, report)[0], 503)
        self.assertEqual(self.received, [])
        evidence = json.loads((self.root / 'compute-a-stack-report.summary.json').read_text())
        self.assertEqual(
            evidence,
            {'id': 17, 'status': 'succeeded', 'roots_sha256': 'proof', 'sha256': hashlib.sha256(report).hexdigest()},
        )
        self.assertEqual(self.request('POST', STACK_POLL, other)[0], 200)
        self.configure()
        self.assertEqual(self.request('POST', STACK_POLL, report)[0], 200)
        self.assertEqual(self.received, [other, report])


class StackReleaseTests(unittest.TestCase):
    def test_create_uses_pair_and_node_and_checks_five_hour_deadline_in_seconds(self):
        harness = object.__new__(StackAcceptance)
        pair = {'id': 'a' * 64, 'source_sha256': None, 'target_sha256': 'b' * 64}
        now = 1795766400
        entry = {
            'id': 17,
            'status': 'preparing',
            'node_id': 'compute-a',
            'source_sha256': pair['source_sha256'],
            'target_sha256': pair['target_sha256'],
            'start_deadline_at': now + 18000,
        }
        clock = SimpleNamespace(stdout=str(now).encode())
        with (
            patch.object(harness, 'remote', return_value=clock),
            patch.object(harness, 'acceptance', create=True) as acceptance,
        ):
            acceptance.call.return_value = entry
            self.assertEqual(harness.create_release(pair), entry)
            acceptance.call.assert_called_once_with(
                'POST', '/cvm/stack-releases', {'node_id': 'compute-a', 'pair_id': pair['id']}, expected=(200, 201, 202)
            )
            for deadline in (None, now + 14400, now + 21600, (now + 18000) * 1000):
                with self.subTest(deadline=deadline):
                    acceptance.call.return_value = {**entry, 'start_deadline_at': deadline}
                    with self.assertRaisesRegex(RuntimeError, 'five-hour server-assigned start deadline'):
                        harness.create_release(pair)


class StackSignatureTests(unittest.TestCase):
    def test_catalog_signs_source_free_install_for_bundle_a_with_the_same_key(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            args = SimpleNamespace(output=root / 'catalog')
            manifests = []
            for role, marker in (('source', 'a'), ('target', 'b')):
                archive = root / role
                archive.write_text(marker)
                setattr(args, role, archive)
                setattr(args, role + '_sha256', hashlib.sha256(marker.encode()).hexdigest())
                manifests.append(
                    {
                        'architecture': 'x86_64',
                        'storage_backends': ['plain'],
                        'state_abi': 'fixture',
                        'files': {
                            cvm_stack.CUBELET: {'sha256': marker * 64},
                            cvm_stack.SHIM: {'sha256': marker * 64},
                            'runtime/cube-shim/version': {'sha256': marker * 64},
                        },
                    }
                )
            with patch.object(cvm_stack, 'inspect_archive', side_effect=manifests):
                make_catalog(args)
            catalog = json.loads((args.output / 'lab-catalog.json').read_text())
            self.assertEqual(set(catalog), {'public', 'source', 'target', 'pairs'})
            self.assertEqual(set(catalog['pairs']), {'forward', 'reverse', 'first-install'})
            public = root / 'public.der'
            public.write_bytes(bytes.fromhex('302a300506032b6570032100' + catalog['public']))
            wire, signature = root / 'wire', root / 'signature'
            for label, pair_id in catalog['pairs'].items():
                wrapper = json.loads((args.output / (pair_id + '.json')).read_text())
                wire.write_bytes(wrapper['payload'].encode())
                signature.write_bytes(bytes.fromhex(wrapper['signature']))
                self.assertEqual(hashlib.sha256(wire.read_bytes()).hexdigest(), pair_id)
                openssl(
                    'pkeyutl',
                    '-verify',
                    '-rawin',
                    '-pubin',
                    '-keyform',
                    'DER',
                    '-inkey',
                    str(public),
                    '-in',
                    str(wire),
                    '-sigfile',
                    str(signature),
                )
                payload = json.loads(wire.read_bytes())
                target, target_marker = (args.target_sha256, 'b') if label == 'forward' else (args.source_sha256, 'a')
                expected = {
                    'contract': 'brocade-cvm-stack-fresh-install-v1',
                    'architecture': 'x86_64',
                    'storage_backend': 'plain',
                    'target_sha256': target,
                    'target_bytes': 1,
                    'target_cubelet_sha256': target_marker * 64,
                    'target_shim_sha256': target_marker * 64,
                }
                if label != 'first-install':
                    source, source_marker = (
                        (args.source_sha256, 'a') if label == 'forward' else (args.target_sha256, 'b')
                    )
                    expected.update(
                        contract='brocade-cvm-stack-cold-pair-v1',
                        source_sha256=source,
                        source_cubelet_sha256=source_marker * 64,
                        source_shim_sha256=source_marker * 64,
                    )
                self.assertEqual(payload, expected)

    def test_empty_receipt_requires_flat_positive_observation_from_owned_compute(self):
        receipt = {'node_id': 'compute-a', 'observed_at': 42, 'installable': 'plain'}
        for invalid in (
            {'node_id': 'compute-a', 'observed_at': 42},
            {**receipt, 'installable': 'xfs'},
            {**receipt, 'bundle_sha256': 'f' * 64},
            {**receipt, 'installed': None},
            {**receipt, 'observed_at': False},
            {**receipt, 'node_id': 'compute-b'},
        ):
            self.assertIsNone(empty_observation({'hosts': [invalid]}))
        self.assertEqual(empty_observation({'hosts': [receipt]}), receipt)

    def test_ed25519_wrapper_signs_exact_utf8_payload_and_filename(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            key, public, wire, signature = [root / name for name in ('key', 'public', 'payload', 'signature')]
            openssl('genpkey', '-algorithm', 'ED25519', '-out', str(key))
            openssl('pkey', '-in', str(key), '-pubout', '-out', str(public))
            pair_id = sign_pair(root, key, {'note': '验收', 'contract': 'fixture'})
            wrapper = json.loads((root / (pair_id + '.json')).read_text())
            payload = b'{"contract":"fixture","note":"' + '验收'.encode() + b'"}'
            self.assertEqual(wrapper['payload'].encode(), payload)
            self.assertEqual(pair_id, hashlib.sha256(payload).hexdigest())
            wire.write_bytes(payload)
            signature.write_bytes(bytes.fromhex(wrapper['signature']))
            arguments = (
                'pkeyutl',
                '-verify',
                '-rawin',
                '-pubin',
                '-inkey',
                str(public),
                '-in',
                str(wire),
                '-sigfile',
                str(signature),
            )
            openssl(*arguments)
            wire.write_bytes(payload + b' ')
            with self.assertRaises(subprocess.CalledProcessError):
                openssl(*arguments)


if __name__ == '__main__':
    unittest.main()
