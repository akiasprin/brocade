#!/usr/bin/env python3
"""Lab-only HTTP delay injection; forwards real Agent bytes without fabricating receipts."""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
import os
from pathlib import Path
import select
import socket
import threading
import time

MAX_BODY = 16 * 1024 * 1024


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--port', type=int, default=18081)
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

        def handle_request(self):
            if self.headers.get('Upgrade', '').lower() == 'websocket':
                # Pass the real authenticated Agent channel byte-for-byte, including its
                # handshake. Delays apply only to HTTP claims/receipts, never fake wake data.
                upstream = socket.create_connection(('127.0.0.1', 8080), timeout=5)
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
            connection = http.client.HTTPConnection('127.0.0.1', 8080, timeout=40)
            try:
                headers = {k: v for k, v in self.headers.items() if k.lower() not in ('host', 'connection')}
                connection.request(self.command, self.path, body=body if size else None, headers=headers)
                response = connection.getresponse()
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
