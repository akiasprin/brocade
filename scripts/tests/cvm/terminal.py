"""Real Console browser WebSocket -> Agent -> Guest PTY acceptance.

Only the explicitly assigned disposable Guest and this run's private Agent state
are used. Requires the local Python websockets package; no host shell is exposed.
"""
import asyncio
import json
from contextlib import asynccontextmanager
import shlex
import subprocess
import time
import urllib.request


def verify(args, broker, admin, node, prefix, created, call, py, record):
    from websockets.legacy.client import connect as websocket_connect
    identity = created['desired']['id']
    host = next(h for h in call('GET', '/cvm')['hosts'] if h['node_id'] == node)
    original_policy = host['terminal_enabled']
    call('PUT', '/cvm/hosts/' + node + '/terminal',
         {'enabled': True, 'expected_revision': host['terminal_revision']}, origin=broker)
    environment = {
        'BROCADE_RUN_CVM_KVM': '1', 'BROCADE_AGENT_SERVER': args.guest_visible_console.format(port=int(broker.rsplit(':', 1)[1])),
        'BROCADE_NODE_TOKEN_FILE': prefix + '/node.token', 'BROCADE_AGENT_STATE_DIR': prefix + '/agent-state',
        'BROCADE_AGENT_APPLY': 'linux', 'BROCADE_CVM_CONFIG': prefix + '/config.json',
        'BROCADE_CVM_STATE_DIR': prefix + '/runtime',
    }
    name = 'cvm::tests::kvm_terminal_service'
    command = 'sudo env ' + ' '.join(shlex.quote(k + '=' + v) for k, v in environment.items())
    command += ' ' + shlex.quote(prefix + '/agent-test') + ' --exact ' + name + ' --ignored --nocapture'
    log_path = args.output.resolve() / 'terminal-agent.log'
    stop = prefix + '/agent-state/terminal-service.stop'
    with log_path.open('wb') as log:
        process = subprocess.Popen(['sh', str(args.ssh_helper), str(args.ssh_port), command], stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 40
            while True:
                # Startup revalidates saved intent before registering control:
                # its same-generation receipt is briefly pending (409), then
                # control may still be unavailable (503). Both must settle.
                ticket = call('POST', '/cvm/' + identity + '/terminal', {}, expected=(200, 409, 503), origin=broker)
                if 'session_id' in ticket:
                    break
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError('production Agent terminal control did not register; see private terminal log')
                time.sleep(.25)

            @asynccontextmanager
            async def connect(ticket):
                path = '/cvm/' + identity + '/terminal/' + ticket['session_id'] + '/stream'
                request = urllib.request.Request(broker + path)
                for handler in admin.handlers:
                    if hasattr(handler, 'cookiejar'):
                        handler.cookiejar.add_cookie_header(request)
                async with websocket_connect('ws' + broker[4:] + path, origin=broker,
                    extra_headers={'Cookie': request.get_header('Cookie', '')}, open_timeout=10, close_timeout=5,
                    max_size=32768, max_queue=4) as socket:
                    await socket.send(json.dumps({'ticket': ticket['ticket']}))
                    yield socket

            async def read_until(socket, expected):
                output = bytearray()
                async with asyncio.timeout(15):
                    while expected not in output:
                        frame = await socket.recv()
                        if isinstance(frame, str):
                            if json.loads(frame)['type'] in ('session', 'ready', 'replay'):
                                continue
                            raise RuntimeError('Guest terminal closed before expected PTY output')
                        output.extend(frame)
                        if len(output) > 65536:
                            raise RuntimeError('Guest terminal acceptance output exceeded limit')
                        await socket.send(json.dumps({'type': 'ack', 'bytes': len(frame)}))
                return bytes(output)

            async def restored(socket):
                terminal_id = None
                async with asyncio.timeout(20):
                    while True:
                        frame = await socket.recv()
                        if isinstance(frame, bytes):
                            await socket.send(json.dumps({'type': 'ack', 'bytes': len(frame)}))
                            continue
                        message = json.loads(frame)
                        if message['type'] == 'session':
                            terminal_id = message['id']
                        elif message['type'] == 'replay' and not message['active']:
                            assert terminal_id is not None
                            return terminal_id
                        elif message['type'] not in ('ready', 'replay'):
                            raise RuntimeError('Guest terminal restore failed')

            async def exercise():
                async with connect(ticket) as socket:
                    terminal_id = await restored(socket)
                    await socket.send(b"stty -echo; PS1='BROCADE_PROMPT> '; printf '\\137\\137ECHO_READY__\\n'\n")
                    await read_until(socket, b'__ECHO_READY__')
                    await socket.send(json.dumps({'type': 'resize', 'cols': 98, 'rows': 34}))
                    await socket.send("stty size; printf '你好，CVM\\n__RESIZED__\\n'\n".encode())
                    output = await read_until(socket, b'__RESIZED__')
                    assert b'34 98' in output and '你好，CVM'.encode() in output
                    await socket.send(b"/bin/sh -c 'printf \"__WAITING__\\n\"; exec sleep 30'\n")
                    await read_until(socket, b'__WAITING__')
                    await socket.send(b'\x03')
                    await read_until(socket, b'BROCADE_PROMPT> ')
                    await socket.send(b"printf '__INTERRUPTED__\\n'\n")
                    await read_until(socket, b'__INTERRUPTED__')
                    await socket.send(b"KEEP_VALUE=persistent; cd /tmp\n")
                    await socket.send(b"printf '__PID_%s_END__\\n' \"$$\"\n")
                    before = await read_until(socket, b'_END__')
                    pid = before.split(b'__PID_')[-1].split(b'_END__')[0]
                    assert pid.isdigit()
                resumed = call('POST', '/cvm/' + identity + '/terminal',
                    {'terminal_id': terminal_id}, origin=broker)
                async with connect(resumed) as socket:
                    assert await restored(socket) == terminal_id
                    await socket.send(b"printf '__AFTER_%s_%s_%s__\\n' \"$$\" \"$KEEP_VALUE\" \"$PWD\"\n")
                    after = await read_until(socket, b'_persistent_/tmp__')
                    assert b'__AFTER_' + pid + b'_persistent_/tmp__' in after
                    await socket.send(b'exit\n')
                    async with asyncio.timeout(15):
                        while True:
                            frame = await socket.recv()
                            if isinstance(frame, bytes):
                                await socket.send(json.dumps({'type': 'ack', 'bytes': len(frame)}))
                                continue
                            result = json.loads(frame)
                            assert result['type'] == 'exit' and result['code'] == 0
                            break
                record('real Console/Agent/Guest PTY supports Unicode, resize, Ctrl-C, exit, ACK and detach/reattach with the same PID, variable and cwd')

            asyncio.run(exercise())
        finally:
            py('import pathlib;pathlib.Path(' + repr(stop) + ').touch(mode=0o600)')
            try:
                code = process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
                raise RuntimeError('terminal acceptance service did not finish owned cleanup')
            py('import pathlib;pathlib.Path(' + repr(stop) + ').unlink()')
            host = next(h for h in call('GET', '/cvm')['hosts'] if h['node_id'] == node)
            call('PUT', '/cvm/hosts/' + node + '/terminal',
                 {'enabled': original_policy, 'expected_revision': host['terminal_revision']}, origin=broker)
    output = log_path.read_bytes()
    if code != 0 or b'1 passed' not in output or (name + ' ... ok').encode() not in output:
        raise RuntimeError('terminal acceptance did not complete exactly one real Agent service test')
    record('owned terminal service exited after the Guest Shell completed')
