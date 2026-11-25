"""Prove that a channel upgrade cannot bypass an HTTP receipt fault."""

import hashlib
import http.client
import json
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest

from release_kvm_lab import Lab


class LabRouteValidationTests(unittest.TestCase):
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


if __name__ == '__main__':
    unittest.main()
