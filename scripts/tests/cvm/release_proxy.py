#!/usr/bin/env python3
"""Lab-only HTTP delay injection; forwards real Agent bytes without fabricating receipts."""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
import os
from pathlib import Path
import re
import select
import socket
import threading
import time

MAX_BODY = 16 * 1024 * 1024
MAX_STACK_BUNDLE = 8 * 1024**3
STACK_BUNDLE = re.compile(r'/agent/v1/cvm/stack-bundles/([a-f0-9]{64})')
STACK_POLL = '/agent/v1/cvm/stack-releases/poll'
STACK_CLAIM = re.compile(r'/agent/v1/cvm/stack-releases/([1-9][0-9]{0,18})/claim')
STACK_ROOTS = re.compile(r'[a-f0-9]{64}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--port', type=int, default=18081)
    parser.add_argument('--upstream-port', type=int, default=8080)
    args = parser.parse_args()
    root = args.root.resolve()
    root.mkdir(mode=0o700, exist_ok=True)
    lock = threading.Lock()

    def private(path, content):
        temporary = path.with_suffix('.tmp')
        with temporary.open('wb') as stream:
            os.chmod(temporary, 0o600)
            stream.write(content)
        temporary.replace(path)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *_):
            pass

        def respond(self, status, body=b'', headers=()):
            self.send_response(status)
            for name, value in headers:
                if name.lower() not in ('content-length', 'connection', 'transfer-encoding'):
                    self.send_header(name, value)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Connection', 'close')
            self.end_headers()
            if body:
                self.wfile.write(body)

        def bundle(self, response, node, mode):
            raw_length = response.getheader('Content-Length', '')
            if not raw_length.isdigit() or not 0 < int(raw_length) <= MAX_STACK_BUNDLE:
                return self.respond(502)
            remaining = int(raw_length)
            if mode == 'size':
                remaining -= 1
            if mode:
                proof = {'mode': mode, 'path': self.path, 'upstream_bytes': int(raw_length)}
                private(root / (node + '-stack-download.summary.json'), json.dumps(proof).encode())
            self.send_response(200)
            for name, value in response.getheaders():
                if name.lower() not in ('content-length', 'connection', 'transfer-encoding'):
                    self.send_header(name, value)
            self.send_header('Content-Length', str(remaining))
            self.send_header('Connection', 'close')
            self.end_headers()
            self.close_connection = True
            flip = mode == 'hash'
            while remaining:
                block = response.read(min(remaining, 65536))
                if not block:
                    return
                if flip:
                    block = bytes([block[0] ^ 1]) + block[1:]
                    flip = False
                try:
                    self.wfile.write(block)
                except (BrokenPipeError, ConnectionResetError):
                    return
                remaining -= len(block)

        def handle_request(self):
            if self.headers.get('Upgrade', '').lower() == 'websocket':
                with lock:
                    control = json.loads((root / 'control.json').read_text())
                if any(
                    control.get(option)
                    for option in (
                        'hold_desired',
                        'hold_cvm_desired',
                        'hold_machine',
                        'hold_nics',
                        'hold_entries',
                        'corrupt_artifact',
                    )
                ):
                    # Fault phases explicitly exercise the production HTTP fallback.
                    # Channel RPCs would otherwise bypass the HTTP receipt gate.
                    return self.respond(503, b'owned lab HTTP fault-injection phase')
                # Pass the real authenticated Agent channel byte-for-byte, including its
                # handshake. Delays apply only to HTTP claims/receipts, never fake wake data.
                upstream = socket.create_connection(('127.0.0.1', args.upstream_port), timeout=5)
                try:
                    upstream.settimeout(None)
                    request = (
                        self.requestline
                        + '\r\n'
                        + ''.join(k + ': ' + v + '\r\n' for k, v in self.headers.items())
                        + '\r\n'
                    )
                    upstream.sendall(request.encode('latin-1'))
                    while True:
                        readable, _, _ = select.select([self.connection, upstream], [], [], 60)
                        for source in readable:
                            data = source.recv(65536)
                            if not data:
                                return
                            (upstream if source is self.connection else self.connection).sendall(data)
                except OSError:
                    return
                finally:
                    self.close_connection = True
                    upstream.close()
            size = int(self.headers.get('Content-Length', '0'))
            if not 0 <= size <= MAX_BODY:
                return self.respond(413)
            body = self.rfile.read(size)
            authorization = self.headers.get('Authorization', '')
            digest = hashlib.sha256(authorization.encode()).hexdigest()
            with lock:
                control = json.loads((root / 'control.json').read_text())
                node = control.get('nodes', {}).get(digest, '')
                stack_download = control.get('stack_download') or {}
                bundle = STACK_BUNDLE.fullmatch(self.path) if self.command == 'GET' else None
                mode = None
                if node and bundle and stack_download.get('node') == node and stack_download.get('sha256') == bundle[1]:
                    if stack_download.get('mode') in ('hash', 'size'):
                        mode = stack_download['mode']
                claim = STACK_CLAIM.fullmatch(self.path) if self.command == 'POST' else None
                if claim and node and node in control.get('hold_stack_claim', []):
                    try:
                        request = json.loads(body)
                    except ValueError:
                        request = None
                    roots = request.get('roots_sha256') if isinstance(request, dict) else None
                    if isinstance(roots, str) and STACK_ROOTS.fullmatch(roots):
                        proof = {'id': int(claim[1]), 'path': self.path, 'roots_sha256': roots}
                        private(root / (node + '-stack-claim.summary.json'), json.dumps(proof).encode())
                        return self.respond(503, b'owned lab stack claim unavailable')
                receipt_gate = control.get('hold_stack_report') or {}
                if self.command == 'POST' and self.path == STACK_POLL and node and receipt_gate.get('node') == node:
                    try:
                        report = json.loads(body).get('report') or {}
                    except (ValueError, AttributeError):
                        report = {}
                    if (
                        isinstance(report, dict)
                        and report.get('id') == receipt_gate.get('id')
                        and report.get('status') == receipt_gate.get('status')
                        and report.get('status') in ('succeeded', 'recovered', 'blocked')
                    ):
                        proof = {key: report.get(key) for key in ('id', 'status', 'roots_sha256')}
                        proof['sha256'] = hashlib.sha256(body).hexdigest()
                        private(root / (node + '-stack-report.summary.json'), json.dumps(proof).encode())
                        return self.respond(503, b'owned lab stack receipt unavailable')
                flag = None
                if self.path.startswith('/agent/v1/cvm/desired') and node in control.get('hold_cvm_desired', []):
                    return self.respond(
                        503, b'{"error":"owned lab compute intent delay"}', [('Content-Type', 'application/json')]
                    )
                if self.path.startswith('/agent/v1/desired') and node in control.get('hold_desired', []):
                    return self.respond(204)
                for path, option, label in [
                    ('/agent/v1/observation', 'hold_machine', 'machine'),
                    ('/agent/v1/cvm/nics/report', 'hold_nics', 'nic'),
                    ('/agent/v1/cvm/entries/report', 'hold_entries', 'entry'),
                ]:
                    if self.path == path and node in control.get(option, []):
                        flag = label
                        break
                if flag:
                    value = json.loads(body)
                    phases = {b['phase'] for b in value.get('bindings', [])}
                    label = flag + ('-withdrawing' if 'withdrawing' in phases else '')
                    path = root / (node + '-' + label + '.json')
                    private(path, body)
                    summary = {
                        'node': node,
                        'kind': label,
                        'generation': value.get('generation'),
                        'sha256': hashlib.sha256(body).hexdigest(),
                        'bytes': len(body),
                        'captured_at': time.time(),
                        'path': self.path,
                    }
                    private(root / (node + '-' + label + '.summary.json'), json.dumps(summary).encode())
                    return self.respond(
                        503, b'{"error":"owned lab receipt delay"}', [('Content-Type', 'application/json')]
                    )
            connection = http.client.HTTPConnection('127.0.0.1', args.upstream_port, timeout=40)
            try:
                headers = {k: v for k, v in self.headers.items() if k.lower() not in ('host', 'connection')}
                connection.request(self.command, self.path, body=body if size else None, headers=headers)
                response = connection.getresponse()
                if bundle and response.status == 200:
                    return self.bundle(response, node, mode)
                data = response.read(MAX_BODY + 1)
                if len(data) > MAX_BODY:
                    return self.respond(502)
                if (
                    response.status == 200
                    and self.path.startswith('/agent/v1/cvm/desired')
                    and node in control.get('corrupt_artifact', [])
                ):
                    value = json.loads(data)
                    for instance in value.get('instances', []):
                        bundle = instance.get('artifacts')
                        if bundle and bundle.get('files'):
                            bundle['files'][0]['content'] += '\nowned-lab-integrity-fault\n'
                    data = json.dumps(value).encode()
                self.respond(response.status, data, response.getheaders())
            except (OSError, http.client.HTTPException):
                self.respond(502)
            finally:
                connection.close()

        do_GET = handle_request
        do_POST = handle_request
        do_PUT = handle_request
        do_DELETE = handle_request

    ThreadingHTTPServer(('0.0.0.0', args.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
