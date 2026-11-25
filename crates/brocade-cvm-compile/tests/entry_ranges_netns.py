"""Exercise generated entry rules exclusively inside disposable network namespaces."""

import json
from pathlib import Path
import selectors
import socket
import subprocess
import sys
import threading


def run(*args):
    subprocess.run(args, check=True, capture_output=True, timeout=5)


def peer():
    print('ready', flush=True)
    sequence = 45000
    for line in sys.stdin:
        request = json.loads(line)
        if request == 'setup':
            run('ip', 'link', 'set', 'lo', 'up')
            run('ip', 'addr', 'add', '198.51.100.20/24', 'dev', 'c0')
            run('ip', 'link', 'set', 'c0', 'up')
            print('ready', flush=True)
            continue
        replies = []
        for protocol in ['tcp', 'udp']:
            for port in [30000, 32048, 34095]:
                kind = socket.SOCK_STREAM if protocol == 'tcp' else socket.SOCK_DGRAM
                with socket.socket(socket.AF_INET, kind) as connection:
                    connection.settimeout(0.4)
                    connection.bind(('198.51.100.20', sequence))
                    sequence += 1
                    try:
                        connection.connect(('198.51.100.10', port))
                        connection.sendall(b'probe')
                        replies.append(connection.recv(32).decode())
                    except (TimeoutError, ConnectionRefusedError):
                        replies.append('blocked')
        print(json.dumps(replies), flush=True)


def serve(selector, stop):
    while not stop.is_set():
        for key, _ in selector.select(timeout=0.1):
            protocol, answer = key.data
            if protocol == 'tcp':
                connection, _ = key.fileobj.accept()
                with connection:
                    connection.settimeout(1)
                    connection.recv(32)
                    connection.sendall(answer.encode())
            else:
                _, address = key.fileobj.recvfrom(32)
                key.fileobj.sendto(answer.encode(), address)


def main(directory):
    run('ip', 'link', 'set', 'lo', 'up')
    run('ip', 'link', 'add', 'r0', 'type', 'veth', 'peer', 'name', 'c0')
    run('ip', 'addr', 'add', '198.51.100.10/24', 'dev', 'r0')
    run('ip', 'link', 'set', 'r0', 'up')
    run('ip', 'addr', 'add', '192.0.2.10/32', 'dev', 'lo')
    client = subprocess.Popen(
        ['unshare', '-n', 'python3', __file__, 'peer'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
    )
    stop = threading.Event()
    selector = selectors.DefaultSelector()
    sockets = []
    thread = None
    try:
        assert client.stdout.readline().strip() == 'ready'
        run('ip', 'link', 'set', 'c0', 'netns', str(client.pid))
        client.stdin.write(json.dumps('setup') + '\n')
        client.stdin.flush()
        assert client.stdout.readline().strip() == 'ready'
        for host, first, prefix in [('198.51.100.10', 30000, 'host'), ('192.0.2.10', 40000, 'target')]:
            for protocol, kind in [('tcp', socket.SOCK_STREAM), ('udp', socket.SOCK_DGRAM)]:
                for port in [first, first + 2048, first + 4095]:
                    listener = socket.socket(socket.AF_INET, kind)
                    listener.bind((host, port))
                    if protocol == 'tcp':
                        listener.listen(4)
                    selector.register(listener, selectors.EVENT_READ, (protocol, f'{prefix}:{port}'))
                    sockets.append(listener)
        thread = threading.Thread(target=serve, args=(selector, stop))
        thread.start()
        for name, expected in [
            ('active', [f'target:{port}' for port in [40000, 42048, 44095]] * 2),
            ('blocked', ['blocked'] * 6),
            ('empty', [f'host:{port}' for port in [30000, 32048, 34095]] * 2),
        ]:
            path = str(directory / (name + '.nft'))
            run('nft', '-c', '-f', path)
            run('nft', '-f', path)
            client.stdin.write(json.dumps('probe') + '\n')
            client.stdin.flush()
            actual = json.loads(client.stdout.readline())
            assert actual == expected, (name, actual, expected)
        print('18 port/protocol checks passed')
    finally:
        client.kill()
        client.wait(timeout=5)
        stop.set()
        if thread:
            thread.join(timeout=3)
        for listener in sockets:
            listener.close()
        selector.close()


if __name__ == '__main__':
    if sys.argv[1] == 'peer':
        peer()
    else:
        main(Path(sys.argv[1]))
