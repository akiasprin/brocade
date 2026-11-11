#!/usr/bin/env python3
"""Build the optional, versioned Cube node runtime from components/cube.

System libraries and BPF development headers are explicit build inputs. Their
digests are recorded with the source, tools, stages and produced artifacts.
This prepares artifacts; it neither installs them nor changes running services.
"""
import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import time

from cvm_elf import DEFAULT_GLIBC, validate as validate_elf


def timestamp():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec='milliseconds')


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        while chunk := stream.read(1024 * 1024):
            h.update(chunk)
    return h.hexdigest()


def source_digest(root):
    result = subprocess.run(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard',
                             '--', 'components/cube', 'scripts/build-cube.py', 'scripts/cvm_elf.py',
                             'scripts/zig-cc', 'scripts/zig-ar'], cwd=root, check=True, capture_output=True)
    h = hashlib.sha256()
    for name in sorted(set(filter(None, result.stdout.decode().split('\0')))):
        path = root / name
        if path.is_file():
            h.update(name.encode() + b'\0' + digest(path).encode() + b'\0')
    return h.hexdigest()


def atomic_json(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(path)


def run(command, cwd, env, record, name, log_dir):
    started, mono = timestamp(), time.monotonic()
    step = {'name': name, 'command': command, 'startedAt': started, 'endedAt': None, 'result': 'running'}
    record['steps'].append(step)
    atomic_json(Path(record['manifest']),record)
    print(f'[{started}] {name}: start', flush=True)
    log_path = log_dir / (name + '.log')
    step['log'] = str(log_path)
    size, tail, last = 0, bytearray(), mono
    with log_path.open('wb') as log:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        try:
            while selector.get_map():
                if time.monotonic() - last >= 30:
                    print(f'[{timestamp()}] {name}: running {time.monotonic()-mono:.0f}s; {log_path}', flush=True)
                    last = time.monotonic()
                    step['elapsedSeconds']=round(last-mono,3)
                    atomic_json(Path(record['manifest']),record)
                for key, _ in selector.select(.5):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    size += len(chunk)
                    if size > 16 * 1024 * 1024:
                        raise RuntimeError(f'{name} output exceeds 16 MiB')
                    log.write(chunk)
                    tail[:] = (tail + chunk)[-4000:]
            code = process.wait()
        except BaseException:
            import signal
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid,signal.SIGKILL)
                    process.wait()
            step.update(result='interrupted', endedAt=timestamp(), durationSeconds=round(time.monotonic()-mono, 3))
            raise
        finally:
            selector.close()
    step.update(result='passed' if code == 0 else 'failed', exitCode=code,
                endedAt=timestamp(), durationSeconds=round(time.monotonic()-mono, 3))
    print(f'[{step["endedAt"]}] {name}: {step["result"]}, {step["durationSeconds"]}s', flush=True)
    atomic_json(Path(record['manifest']),record)
    if code:
        raise RuntimeError(f'{name} failed; {log_path}\n{tail.decode(errors="replace")}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=['x86_64', 'aarch64'], required=True)
    parser.add_argument('--jobs',type=int,choices=range(2,9),default=4)
    parser.add_argument('--sysroot-libs', type=Path, required=True,
                        help='matching target-architecture libcap-ng.so and libseccomp.so link inputs')
    parser.add_argument('--bpf-include', type=Path, default=Path('/usr/include'),
                        help='directory containing bpf/bpf_helpers.h')
    parser.add_argument('--glibc', default=DEFAULT_GLIBC, help='GNU shim libc baseline used by the matching sysroot')
    parser.add_argument('--clang', type=Path, default=Path('/usr/lib/llvm-21/bin/clang'))
    parser.add_argument('--go', default=os.environ.get('BROCADE_GO', 'go'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if str(root).startswith(('/tmp/', '/var/tmp/')):
        parser.error('use a persistent checkout')
    libraries = args.sysroot_libs.resolve()
    dependencies = [libraries / 'libcap-ng.so', libraries / 'libseccomp.so']
    for path in [*dependencies, args.clang, args.bpf_include / 'bpf/bpf_helpers.h']:
        if not path.is_file():
            parser.error(f'required build input is missing: {path}')
    try:
        requirements = {path.name: validate_elf(path, args.arch, args.glibc) for path in dependencies}
    except ValueError as error:
        parser.error(str(error))
    go = shutil.which(args.go) if not Path(args.go).is_absolute() else args.go
    if not go:
        parser.error('Go compiler is missing')
    version = subprocess.check_output([go, 'version'], text=True).strip()
    if 'go1.27.3 ' not in version:
        parser.error('this runtime recipe requires Go 1.27.3')
    cache = root / 'target/cube'
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / 'build.lock').open('w') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            parser.error('another Cube runtime build owns this checkout')
        output = cache / 'artifacts' / args.arch
        output.mkdir(parents=True, exist_ok=True)
        logs = output / 'logs'
        logs.mkdir(exist_ok=True)
        manifest = output / 'manifest.json'
        started, mono = timestamp(), time.monotonic()
        record = {'formatVersion': 1, 'startedAt': started, 'endedAt': None, 'result': 'running',
                  'manifest':str(manifest),
                  'deploymentReady': False, 'architecture': args.arch, 'steps': [], 'artifacts': {},
                  'sourceSHA256': source_digest(root), 'protocols': ['protected-root-v1', 'cubevs-attachments-v2', 'guest-pty-v1'],
                  'tools': {'go': version, 'rust': '1.96.0', 'clang': str(args.clang), 'gnuLibcBaseline': args.glibc},
                  'nativeLibraryInputs': {str(p): digest(p) for p in dependencies},
                  'elfRequirements': {'nativeLibraries': requirements, 'artifacts': {}}}
        atomic_json(manifest, record)
        env = os.environ.copy()
        env['PATH'] = str(args.clang.parent) + os.pathsep + str(Path(go).parent) + os.pathsep + env.get('PATH', '')
        env['CPATH'] = str(args.bpf_include.resolve())
        env['GOARCH'] = 'amd64' if args.arch == 'x86_64' else 'arm64'
        musl = args.arch + '-unknown-linux-musl'
        gnu = args.arch + '-unknown-linux-gnu'
        cc = str(root / 'scripts/zig-cc')
        env['CC'] = cc
        env['CARGO_BUILD_JOBS']=str(args.jobs)
        try:
            run([go, 'generate', 'cubevs.go'], root / 'components/cube/CubeNet/cubevs', env, record, 'bpf', logs)
            env['CARGO_TARGET_DIR'] = str(cache / 'cow')
            env['BROCADE_ZIG_TARGET'] = args.arch + '-linux-musl'
            env['CARGO_TARGET_' + musl.upper().replace('-', '_') + '_LINKER'] = cc
            run(['cargo', '+1.96.0', 'rustc', '--manifest-path', 'components/cube/cubecow/Cargo.toml',
                 '--release', '--locked', '--lib', '--crate-type', 'staticlib', '--target', musl],
                root, env, record, 'cow', logs)
            sdk = root / 'components/cube/Cubelet/third_party/cubecow'
            (sdk / 'include').mkdir(parents=True, exist_ok=True)
            (sdk / 'lib').mkdir(exist_ok=True)
            shutil.copy2(cache / 'cow' / musl / 'release/libcubecow.a', sdk / 'lib/libcubecow.a')
            shutil.copy2(root / 'components/cube/cubecow/include/cubecow.h', sdk / 'include/cubecow.h')
            env['CGO_ENABLED'], env['CGO_LDFLAGS'] = '1', '-lunwind'
            run([go, 'build', '-trimpath', '-buildvcs=false', '-ldflags', '-linkmode external -extldflags -static',
                 '-o', str(output / 'cubelet'), './cmd/cubelet'], root / 'components/cube/Cubelet', env, record, 'cubelet', logs)
            env['CARGO_TARGET_DIR'] = str(cache / 'shim')
            env['BROCADE_ZIG_TARGET'] = args.arch + '-linux-gnu.' + args.glibc
            env['CARGO_TARGET_' + gnu.upper().replace('-', '_') + '_LINKER'] = cc
            for package, binary in [('containerd-shim-cube-rs', 'containerd-shim-cube-rs'), ('cube-runtime', 'cube-runtime')]:
                run(['cargo', '+1.96.0', 'rustc', '--manifest-path', 'components/cube/CubeShim/Cargo.toml',
                     '-p', package, '--bin', binary, '--release', '--locked', '--target', gnu,
                     '--', '-L', 'native=' + str(libraries)], root, env, record, binary, logs)
                shutil.copy2(cache / 'shim' / gnu / 'release' / binary, output / binary)
            env['CGO_ENABLED'] = '0'
            run([go, 'build', '-trimpath', '-buildvcs=false', '-o', str(output / 'lan-inspect'), './cmd/lan-inspect'],
                root / 'components/cube/CubeNet/cubevs', env, record, 'inspector', logs)
            if source_digest(root) != record['sourceSHA256']:
                raise RuntimeError('runtime source changed during build; artifacts are not accepted')
            for name in ['cubelet', 'containerd-shim-cube-rs', 'cube-runtime', 'lan-inspect']:
                record['elfRequirements']['artifacts'][name] = validate_elf(output / name, args.arch, args.glibc)
                record['artifacts'][name] = {'sha256': digest(output / name), 'bytes': (output / name).stat().st_size}
            record['shimVersion'] = 'v0.7.3-brocade.cubevs.sha' + record['artifacts']['containerd-shim-cube-rs']['sha256'][:12]
            record['result'] = 'build_complete'
        except BaseException as error:
            record['result'] = 'cancelled' if isinstance(error,KeyboardInterrupt) else 'failed'
            record['error'] = str(error)
            raise
        finally:
            record['endedAt'] = timestamp()
            record['durationSeconds'] = round(time.monotonic() - mono, 3)
            atomic_json(manifest, record)
        print(manifest, flush=True)


if __name__ == '__main__':
    main()
