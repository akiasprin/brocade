#!/usr/bin/env python3
"""Exercise real Agent replacement and receipt replay in a disposable KVM.

Usage: BROCADE_RUN_CVM_KVM=1 python3 selfupdate_replay.py OLD_AGENT NEW_AGENT
Only a private temporary installation and a loopback HTTP fixture are used.
"""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def wait_for(predicate, label, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.2)
    raise AssertionError(f'timed out: {label}')


def main():
    assert os.environ.get('BROCADE_RUN_CVM_KVM') == '1', 'select a disposable KVM explicitly'
    old, candidate = map(lambda value: Path(value).resolve(), sys.argv[1:])
    before, after = digest(old), digest(candidate)
    assert before != after, 'replacement must contain different real binary bytes'
    assert candidate.stat().st_size <= 128 * 1024**2, 'use a release binary or strip debug symbols first'
    observed = threading.Event()
    acknowledge = threading.Event()
    accepted = threading.Event()
    reoffer = threading.Event()
    reports = []
    downloads = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, code, value=None):
            body = b'' if value is None else json.dumps(value).encode()
            self.send_response(code)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == '/candidate':
                downloads.append(time.monotonic())
                self.send_response(200)
                self.send_header('Content-Length', str(candidate.stat().st_size))
                self.end_headers()
                with candidate.open('rb') as stream:
                    shutil.copyfileobj(stream, self.wfile)
            elif self.path == '/agent/v1/agent-release' and (not accepted.is_set() or reoffer.is_set()):
                reoffer.clear()
                self.reply(
                    200,
                    {
                        'flavor': 'cvm',
                        'allow_flavor_change': False,
                        'url': f'http://127.0.0.1:{server.server_port}/candidate',
                        'sha256': after,
                        'previous_sha256': before,
                        'release_id': 1,
                        'attempt': 1,
                    },
                )
            else:
                self.reply(204)

        def do_POST(self):
            size = int(self.headers.get('Content-Length', 0))
            assert size < 1024 * 1024
            body = self.rfile.read(size)
            if self.path == '/agent/v1/agent-release/report':
                reports.append(json.loads(body))
                observed.set()
                if acknowledge.is_set():
                    accepted.set()
                    self.reply(200, {'accepted': True})
                else:
                    self.reply(503)
            else:
                self.reply(200, {})

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='brocade-update-kvm-') as temporary:
        root = Path(temporary)
        target = root / 'brocade-agent'
        shutil.copy2(old, target)
        state = root / 'state'
        command = [
            str(target),
            'run',
            '--server',
            f'http://127.0.0.1:{server.server_port}',
            '--token',
            'disposable-kvm-fixture',
            '--state-dir',
            str(state),
            '--apply',
            'state-dir',
        ]
        processes = []
        with (root / 'agent.log').open('w') as log:

            def start():
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                processes.append(process)
                return process

            try:
                first = start()
                wait_for(lambda: first.poll() is not None, 'old process yields to replacement')
                assert first.returncode == 0
                assert digest(target) == after
                receipt = state / 'agent-release-pending.json'
                assert json.loads(receipt.read_text())['report'] is None
                second = start()
                wait_for(observed.is_set, 'new process confirms installed/running identity')
                frozen = json.loads(receipt.read_text())['report']
                assert frozen['outcome'] == 'running' and frozen['performed_update']
                assert frozen['installed_sha256'] == frozen['running_sha256'] == after
                second.kill()
                second.wait()
                acknowledge.set()
                third = start()
                wait_for(accepted.is_set, 'receipt replay accepted')
                wait_for(lambda: not receipt.exists(), 'acknowledged receipt removed')
                assert reports[-1] == frozen and len(reports) >= 2
                # Re-deliver the same offer after another real process restart.
                inode = target.stat().st_ino
                third.terminate()
                third.wait(timeout=20)
                count = len(reports)
                reoffer.set()
                fourth = start()
                wait_for(lambda: len(reports) > count, 'same offer observes existing installation')
                assert reports[-1]['outcome'] == 'running' and not reports[-1]['performed_update']
                assert fourth.poll() is None and target.stat().st_ino == inode
                assert digest(target) == after and len(downloads) == 1
                print(
                    json.dumps(
                        {
                            'result': 'passed',
                            'before_sha256': before,
                            'after_sha256': after,
                            'downloads': len(downloads),
                            'receipt_attempts': len(reports),
                            'checks': [
                                'real replacement',
                                'new-process confirmation',
                                'SIGKILL before acknowledgement',
                                'durable receipt replay',
                                'no duplicate installation',
                            ],
                        }
                    ),
                    flush=True,
                )
            except BaseException:
                log.flush()
                print((root / 'agent.log').read_text()[-12000:], file=sys.stderr)
                raise
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=20)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                server.shutdown()


if __name__ == '__main__':
    main()
