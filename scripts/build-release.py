#!/usr/bin/env python3
"""Timed dual-architecture GNU build from a stable, exclusively owned checkout."""
import argparse
import contextlib
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import subprocess
import sys
import time


class BuildError(RuntimeError):
    pass


MAX_LOG_BYTES = 16 * 1024 * 1024
PROGRESS_INTERVAL_SECONDS = 30


def timestamp():
    return datetime.now(timezone.utc).isoformat(timespec='milliseconds')


class BuildTimer:
    def __init__(self, started=None):
        self.started = time.monotonic() if started is None else started
        self.started_at = timestamp()
        self.ended_at = None
        self.stage = None
        self.stage_started = self.started
        self.stage_started_at = self.started_at
        self.next_report = time.monotonic() + PROGRESS_INTERVAL_SECONDS
        self.on_progress = None

    def report(self, status='running'):
        now = time.monotonic()
        print(f'[{timestamp()}] release build: {self.stage or "preflight"} {status}, '
              f'started {self.stage_started_at}, '
              f'stage {now - self.stage_started:.1f}s, total {now - self.started:.1f}s',
              file=sys.stderr, flush=True)
        if self.on_progress:
            self.on_progress()
        self.next_report = now + PROGRESS_INTERVAL_SECONDS

    def tick(self):
        if time.monotonic() >= self.next_report:
            self.report()


def stable_root(root):
    if root.is_relative_to(Path('/tmp')) or root.is_relative_to(Path('/var/tmp')):
        raise BuildError('use a persistent checkout, not a temporary directory')
    if any(a == 'target' and b == 'releases' for a, b in zip(root.parts, root.parts[1:])):
        raise BuildError('release archives are outputs, not compilation workspaces')


def run(args, root, env, timer, log=None):
    timer.tick()
    with subprocess.Popen(args, cwd=root, env=env, start_new_session=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT) as process:
        output = bytearray()
        received = 0
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    timer.tick()
                    if not selector.select(timeout=0.1):
                        continue
                    chunk = os.read(process.stdout.fileno(), 65536)
                    if not chunk:
                        break
                    received += len(chunk)
                    if received > MAX_LOG_BYTES:
                        raise BuildError('build output exceeds 16 MiB limit')
                    if log:
                        log.write(chunk.decode(errors='replace'))
                        output[:] = (output + chunk)[-1500:]
                    else:
                        output.extend(chunk)
            while process.poll() is None:
                timer.tick()
                time.sleep(0.1)
        except BaseException:
            # Kill the compiler/build-script process group, not merely the Cargo parent.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            raise
        if process.returncode:
            detail = output.decode(errors='replace')[-1500:]
            raise BuildError(f'{Path(args[0]).name} exited {process.returncode}: {detail}')
        return output.decode().strip()


def digest(path, timer):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        while chunk := stream.read(1024 * 1024):
            timer.tick()
            h.update(chunk)
    return h.hexdigest()


def inputs(root, env, timer, frontend_only=False):
    files = run(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'],
                root, env, timer).split('\0')
    h = hashlib.sha256()
    for name in sorted(set(filter(None, files))):
        if frontend_only and not any(name == prefix or name.startswith(prefix + '/') for prefix in [
                'frontend/src', 'frontend/public', 'frontend/index.html', 'frontend/package.json',
                'frontend/package-lock.json', 'frontend/vite.config.ts', 'frontend/tsconfig.json']):
            continue
        path = root / name
        if path.is_symlink():
            value = 'symlink:' + os.readlink(path)
        elif path.is_file():
            value = f'{path.stat().st_mode & 0o777:o}:' + digest(path, timer)
        else:
            value = 'missing'
        h.update(name.encode() + b'\0' + value.encode() + b'\0')
    if not frontend_only:
        head = run(['git', 'rev-parse', 'HEAD'], root, env, timer)
        h.update(head.encode())
    return h.hexdigest()


def assets_digest(directory, timer):
    h = hashlib.sha256()
    for path in sorted(directory.rglob('*')):
        if path.is_symlink():
            raise BuildError('frontend artifact cache must not contain symlinks')
        if path.is_file():
            h.update(str(path.relative_to(directory)).encode() + b'\0' + digest(path, timer).encode())
    return h.hexdigest()


def frontend(root, state, env, timer):
    tools = {tool: run([tool, '--version'], root, env, timer) for tool in ['node', 'npm']}
    source = inputs(root, env, timer, frontend_only=True)
    stamp = state / 'frontend.json'
    dist = state / 'frontend-dist'
    previous = json.loads(stamp.read_text()) if stamp.exists() else {}
    if (previous.get('source') == source and previous.get('tools') == tools
            and (dist / 'index.html').is_file()
            and previous.get('assets') == assets_digest(dist, timer)):
        return dist
    # Prepare once before the two Console build scripts; concurrent npm ci would race
    # while replacing their shared node_modules directory.
    for label, command in [
        ('frontend-deps', ['npm', 'ci', '--no-audit', '--no-fund']),
        ('frontend-build', ['npm', 'run', 'build', '--', '--outDir', str(dist), '--emptyOutDir']),
    ]:
        with (state / (label + '.log')).open('w') as log:
            run(command, root / 'frontend', env, timer, log)
    value = {'source': source, 'tools': tools, 'assets': assets_digest(dist, timer)}
    temporary = stamp.with_suffix('.tmp')
    temporary.write_text(json.dumps(value) + '\n')
    temporary.replace(stamp)
    return dist


def agent_wrappers(state, zig):
    directory = state.parent / 'brocade-agent-build'
    directory.mkdir(parents=True, exist_ok=True)
    paths = {}
    for arch, target in [('x86_64', 'x86_64-linux-musl'), ('aarch64', 'aarch64-linux-musl')]:
        cc = directory / f'zig-cc-{arch}'
        ar = directory / f'zig-ar-{arch}'
        bodies = {
            cc: f'''#!/bin/sh
set -eu
n=$#
i=0
while [ "$i" -lt "$n" ]; do
  argument=$1
  shift
  i=$((i + 1))
  case "$argument" in --target=*|-nostdlibinc) continue ;; esac
  set -- "$@" "$argument"
done
exec "{zig}" cc -target {target} "$@"
''',
            ar: f'''#!/bin/sh
set -eu
exec "{zig}" ar "$@"
''',
        }
        for path, body in bodies.items():
            if not path.exists() or path.read_text() != body:
                temporary = path.with_suffix('.tmp')
                temporary.write_text(body)
                temporary.chmod(0o700)
                temporary.replace(path)
        paths[arch] = (cc, ar)
    return directory, paths


def agents(root, state, env, timer, jobs, zig):
    target, wrappers = agent_wrappers(state, zig)
    triples = [('x86_64', 'x86_64-unknown-linux-musl'), ('aarch64', 'aarch64-unknown-linux-musl')]
    command = ['cargo', 'build', '--release', '--locked', '--jobs', str(jobs),
               '-p', 'brocade-agent', '--target-dir', str(target)]
    for _, triple in triples:
        command += ['--target', triple]
    agent_env = env.copy()
    for arch, triple in triples:
        key = triple.upper().replace('-', '_')
        cc, ar = wrappers[arch]
        agent_env[f'CC_{triple.replace("-", "_")}'] = str(cc)
        agent_env[f'AR_{triple.replace("-", "_")}'] = str(ar)
        if arch == 'aarch64':
            agent_env[f'CARGO_TARGET_{key}_RUSTFLAGS'] = '-C linker=rust-lld'
    with (state / 'agent.log').open('w') as log:
        run(command, root, agent_env, timer, log)
    for arch, triple in triples:
        binary = target / triple / 'release/brocade-agent'
        env[f'BROCADE_AGENT_BIN_{arch.upper()}'] = str(binary)
    return command


def go_binary(root, env, timer):
    version = (root / 'components/xray-core/.go-version').read_text().strip()
    explicit = env.get('BROCADE_GO')
    machine = {'x86_64': 'amd64', 'aarch64': 'arm64'}.get(os.uname().machine)
    candidates = [explicit] if explicit else [
        shutil.which('go'),
        str(Path.home() / f'go/pkg/mod/golang.org/toolchain@v0.0.1-go{version}.linux-{machine}/bin/go'),
    ]
    for candidate in filter(None, candidates):
        if not Path(candidate).is_file():
            continue
        actual = run([candidate, 'version'], root, env, timer)
        if f' go{version} ' in actual:
            return candidate
    raise BuildError(f'Go {version} is required; set BROCADE_GO to the installed compiler')


def build(args, root, timer):
    print(f'[{timer.started_at}] release build started', file=sys.stderr, flush=True)
    stable_root(root)
    state = root / 'target/brocade-release'
    state.mkdir(parents=True, exist_ok=True)
    record = {'status': 'running', 'build_complete': False, 'deployment_ready': False,
              'started_at': timer.started_at, 'ended_at': None, 'stages': [], 'artifacts': {}}
    with (state / 'lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise BuildError('another release build owns this checkout; no hidden lock queue') from None
        def save_record():
            if record['ended_at'] is None:
                record['elapsed_seconds'] = time.monotonic() - timer.started
            if timer.stage:
                record['active_stage'] = {'name': timer.stage,
                                          'started_at': timer.stage_started_at, 'ended_at': None,
                                          'status': 'running',
                                          'seconds': time.monotonic() - timer.stage_started}
            else:
                record.pop('active_stage', None)
            temporary = state / 'preparation.json.tmp'
            temporary.write_text(json.dumps(record, indent=2) + '\n')
            temporary.replace(state / 'preparation.json')

        @contextlib.contextmanager
        def stage(name):
            timer.stage = name
            timer.stage_started = time.monotonic()
            timer.stage_started_at = timestamp()
            status = 'failed'
            try:
                timer.report('started')
                yield
                status = 'complete'
            except KeyboardInterrupt:
                status = 'cancelled'
                raise
            finally:
                seconds = time.monotonic() - timer.stage_started
                ended_at = timestamp()
                record['stages'].append({'name': name, 'seconds': seconds, 'status': status,
                                         'started_at': timer.stage_started_at, 'ended_at': ended_at})
                timer.stage = None
                save_record()
                print(f'[{ended_at}] release build: {name} {status}, '
                      f'started {timer.stage_started_at}, ended {ended_at}, stage {seconds:.1f}s, '
                      f'total {record["elapsed_seconds"]:.1f}s', file=sys.stderr, flush=True)

        timer.on_progress = save_record
        # Never expose the previous successful record while a new build is in progress.
        save_record()
        try:
            with stage('preflight'):
                env = os.environ.copy()
                for key in ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR',
                            'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES']:
                    env.pop(key, None)
                actual = run(['git', 'rev-parse', '--show-toplevel'], root, env, timer)
                if Path(actual).resolve() != root:
                    raise BuildError('the stable checkout must have its own Git metadata')
                if any(env.get('BROCADE_AGENT_BIN_' + arch) for arch in ['X86_64', 'AARCH64']):
                    raise BuildError('this entry builds current Agents; remove external Agent overrides')
                env.pop('BROCADE_CONSOLE_ASSETS_DIR', None)
                env['BROCADE_GO'] = go_binary(root, env, timer)
                for tool in ['cargo', 'rustc', 'npm', 'node']:
                    if not shutil.which(tool):
                        raise BuildError(f'missing build prerequisite: {tool}')
                zig = env.get('BROCADE_ZIG') or shutil.which('zig')
                if not zig:
                    raise BuildError('missing Zig; set BROCADE_ZIG to the installed compiler')
                env['BROCADE_ZIG'] = zig
                if not run([zig, 'version'], root, env, timer).startswith('0.16.'):
                    raise BuildError('Zig 0.16 is required')
                sysroot = Path(run(['rustc', '--print', 'sysroot'], root, env, timer))
                triples = ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu']
                for triple in triples + ['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl']:
                    if not (sysroot / 'lib/rustlib' / triple / 'lib').is_dir():
                        raise BuildError(f'missing Rust target: {triple}')
                if os.uname().machine != 'x86_64':
                    raise BuildError('this GNU release entry currently requires an x86_64 Linux builder')
                linker = env.get('CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER', 'aarch64-linux-gnu-gcc')
                if not shutil.which(linker):
                    raise BuildError(f'missing cross linker: {linker}')
                env['CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER'] = linker
                env['CARGO_TARGET_DIR'] = str(state / 'cargo')
                env['CARGO_BUILD_JOBS'] = str(args.jobs)
                source = inputs(root, env, timer)
                record['source_sha256'] = source
            with stage('frontend'):
                env['BROCADE_CONSOLE_ASSETS_DIR'] = str(frontend(root, state, env, timer))
            with stage('dual_architecture_agent'):
                record['agent_command'] = agents(root, state, env, timer, args.jobs, zig)
            command = ['cargo', 'build', '--release', '--locked', '--jobs', str(args.jobs),
                       '-p', 'brocade-console', '-p', 'brocade-launcher']
            for triple in triples:
                command += ['--target', triple]
            record['command'] = command
            with stage('dual_architecture_build'):
                with (state / 'cargo.log').open('w') as log:
                    run(command, root, env, timer, log)
            with stage('verification'):
                if inputs(root, env, timer) != source:
                    raise BuildError('source changed during the build; artifacts are not accepted')
                for triple in triples:
                    for name in ['brocade-console', 'brocade-launcher']:
                        path = state / 'cargo' / triple / 'release' / name
                        record['artifacts'][f'{triple}/{name}'] = {'path': str(path), 'sha256': digest(path, timer)}
            record.update(status='built', build_complete=True,
                          remaining_gates=['impact-scoped validation', 'mandatory upgrade acceptance'])
        except BaseException as error:
            record.update(status='cancelled' if isinstance(error, KeyboardInterrupt) else 'failed',
                          error=str(error) or 'interrupted')
            raise
        finally:
            record['elapsed_seconds'] = time.monotonic() - timer.started
            timer.ended_at = timestamp()
            record['ended_at'] = timer.ended_at
            timer.on_progress = None
            save_record()
            print(f'[{record["ended_at"]}] release build {record["status"]}, '
                  f'started {record["started_at"]}, ended {record["ended_at"]}, '
                  f'total {record["elapsed_seconds"]:.1f}s', file=sys.stderr, flush=True)
            print(json.dumps(record), flush=True)


def main():
    timer = BuildTimer()
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--jobs', type=int, choices=range(2, 9), default=4)
    args = parser.parse_args()
    status = 'failed'
    try:
        build(args, Path(__file__).resolve().parents[1], timer)
    except KeyboardInterrupt:
        status = 'cancelled'
        print('release build interrupted', file=sys.stderr)
        return 130
    except (BuildError, OSError) as error:
        print(f'release build failed: {error}', file=sys.stderr)
        return 1
    finally:
        # A rejected checkout/lock must leave the owner's record intact but still close this attempt.
        if timer.ended_at is None:
            elapsed = time.monotonic() - timer.started
            timer.ended_at = timestamp()
            print(f'[{timer.ended_at}] release build {status}, '
                  f'started {timer.started_at}, ended {timer.ended_at}, total {elapsed:.1f}s',
                  file=sys.stderr, flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
