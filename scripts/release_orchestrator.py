#!/usr/bin/env python3
"""Run CI-aligned release builds and owned KVM acceptance without production changes."""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
import hashlib
import fcntl
import json
import os
from pathlib import Path
import re
import selectors
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time


ROOT = Path(__file__).resolve().parents[1]
DOCKERFILE = ROOT / 'scripts/release-environment/Dockerfile'
HEX64 = re.compile(r'[a-f0-9]{64}')
MAIN_PHASES = (
    'bootstrap',
    'publish_barriers',
    'finish_initial',
    'integrity_failure',
    'retry_integrity',
    'verify_retry_fix',
    'networks',
    'lifecycle',
    'mixed_mtu_route',
    'capacity_routes',
    'permissions',
    'entries',
    'traffic_quotas',
    'transport_failure',
    'delete_and_empty',
    'cleanup',
)
STACK_SCENARIOS = ('bad-hash', 'bad-size', 'cancel', 'activation-failure', 'manager-death', 'success', 'rollback')


class OrchestrationError(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise OrchestrationError(message)


def utc_now():
    return datetime.now(timezone.utc).isoformat(timespec='milliseconds')


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.chmod(0o600)
    temporary.replace(path)


def run(command, **kwargs):
    return subprocess.run([str(item) for item in command], capture_output=True, check=False, **kwargs)


def checked(command, **kwargs):
    result = run(command, **kwargs)
    if result.returncode:
        detail = (result.stdout + result.stderr).decode(errors='replace')[-4000:]
        raise OrchestrationError(f'{Path(str(command[0])).name} exited {result.returncode}: {detail}')
    return result


def resolve_path(value, base):
    path = Path(value).expanduser()
    return (base / path if not path.is_absolute() else path).resolve()


def read_config(filename):
    filename = filename.resolve()
    value = json.loads(filename.read_text())
    require(isinstance(value, dict) and value.get('version') == 1, 'environment version must be 1')
    base = filename.parent
    result = {'config_file': str(filename)}
    paths = value.get('paths')
    require(isinstance(paths, dict), 'paths must be an object')
    for key in ('workspace', 'evidence', 'cache'):
        require(isinstance(paths.get(key), str), f'paths.{key} is required')
        result[key] = resolve_path(paths[key], base)
    result['revision'] = value.get('revision', 'HEAD')
    require(re.fullmatch(r'[A-Za-z0-9_./@{}^-]+', result['revision']) is not None, 'invalid source revision')
    for key in ('base_image', 'image_prefix'):
        require(isinstance(value.get(key), str) and value[key], f'{key} is required')
        result[key] = value[key]
    require(re.fullmatch(r'[a-z0-9][a-z0-9._/-]*', result['image_prefix']) is not None, 'invalid image_prefix')
    defaults = {'base': 16, 'cvm': 16, 'cube-a': 12, 'cube-b': 12}
    result['jobs'] = defaults
    require(isinstance(value.get('jobs', {}), dict), 'jobs must be an object')
    for name in defaults:
        jobs = value.get('jobs', {}).get(name, defaults[name])
        require(type(jobs) is int and 1 <= jobs <= 128, f'jobs.{name} must be in 1..128')
        result['jobs'][name] = jobs
    for key, default in (('minimum_cpus', 24), ('minimum_free_gib', 100), ('reserved_memory_gib', 8)):
        selected = value.get(key, default)
        require(type(selected) is int and selected >= 0, f'{key} must be a nonnegative integer')
        result[key] = selected
    result['tools_directory'] = resolve_path(value.get('tools_directory', result['cache'] / 'tools'), base)
    for key in ('tools', 'inputs'):
        require(isinstance(value.get(key, {}), dict), f'{key} must be an object')
        result[key] = {name: resolve_path(item, base) for name, item in value.get(key, {}).items()}
    result['stack_b_patch'] = value.get('stack_b_patch')
    if result['stack_b_patch']:
        result['stack_b_patch'] = resolve_path(result['stack_b_patch'], base)
    result['node_version'] = value.get('node_version')
    require(isinstance(result['node_version'], str), 'node_version must explicitly select a Node 22 release')
    result['kvm_memory_gib'] = value.get('kvm_memory_gib', 33)
    require(
        type(result['kvm_memory_gib']) is int and 0 <= result['kvm_memory_gib'] <= 256, 'invalid KVM memory reservation'
    )
    result['source_repository'] = resolve_path(value.get('source_repository', ROOT), base)
    result['install_missing_tools'] = value.get('install_missing_tools', True)
    require(type(result['install_missing_tools']) is bool, 'install_missing_tools must be Boolean')
    for key in ('os_image_sha256', 'geoip_sha256', 'geosite_sha256'):
        selected = value.get(key)
        require(
            selected is None or (isinstance(selected, str) and HEX64.fullmatch(selected) is not None),
            f'{key} must be a lowercase SHA-256',
        )
        result[key] = selected
    result['acceptance_selector'] = value.get('acceptance_selector', 'all')
    require(result['acceptance_selector'] in ('all', 'main', 'stack', 'first-install'), 'invalid acceptance_selector')
    result['stop_labs'] = value.get('stop_labs', True)
    require(type(result['stop_labs']) is bool, 'stop_labs must be Boolean')
    result['run_directory'] = resolve_path(value.get('run_directory', result['evidence'] / 'kvm-current'), base)
    for name in ('workspace', 'evidence', 'cache', 'run_directory'):
        require(result[name] != result['source_repository'], f'{name} must be outside the source repository')
        require(
            result[name]
            not in (result[other] for other in ('workspace', 'evidence', 'cache', 'run_directory') if other != name),
            'workspace, evidence, cache and run directory must be distinct',
        )
    result['tool_sha256'] = value.get('tool_sha256', {})
    require(isinstance(result['tool_sha256'], dict), 'tool_sha256 must be an object')
    for name in ('node', 'go'):
        selected = result['tool_sha256'].get(name)
        require(
            isinstance(selected, str) and HEX64.fullmatch(selected) is not None,
            f'tool_sha256.{name} must pin the Linux archive',
        )
    return result


def pins(root=ROOT, node_version='22.23.5'):
    toolchain = (root / 'rust-toolchain.toml').read_text()
    build = (root / 'crates/brocade-console/build.rs').read_text()
    result = {
        'rust': re.search(r'^channel = "([^"]+)"', toolchain, re.MULTILINE).group(1),
        'go': (root / 'components/xray-core/.go-version').read_text().strip(),
        'zig_version': re.search(r'^const ZIG_PINNED_VERSION: &str = "([^"]+)";', build, re.MULTILINE).group(1),
        'zig_sha256': re.search(r'^const ZIG_PINNED_SHA256: &str = "([^"]+)";', build, re.MULTILINE).group(1),
        'node': node_version,
    }
    require(
        re.fullmatch(r'22\.\d+\.\d+', result['node']) is not None,
        'node_version must select an explicit Node 22 release',
    )
    require(result['go'] == (root / 'components/cube/.go-version').read_text().strip(), 'Go component pins differ')
    require(HEX64.fullmatch(result['zig_sha256']) is not None, 'invalid Zig digest')
    return result


def tool_roots(config):
    versions = pins(node_version=config['node_version'])
    root = config['tools_directory']
    discovered = config.get('tools', {})
    return {
        'rustup': discovered.get('rustup', root / 'rustup'),
        'cargo': discovered.get('cargo', root / 'cargo'),
        'node': discovered.get('node', root / ('node-v' + versions['node'])),
        'go': discovered.get('go', root / ('go-go' + versions['go'])),
        'zig': discovered.get('zig', root / ('zig-' + versions['zig_version'])),
    }


def wait_all(futures, label):
    failures = []
    for future in as_completed(futures):
        try:
            future.result()
        except BaseException as error:
            failures.append(f'{type(error).__name__}: {error}')
    if failures:
        raise OrchestrationError(f'{label} failed ({len(failures)} suites): ' + '; '.join(failures))


class Runner:
    def __init__(self, directory, label):
        self.directory = directory
        self.directory.mkdir(parents=True, exist_ok=True)
        self.label = label
        self.log = self.directory / 'execution.log'
        self.record_path = self.directory / 'result.json'
        self.started_at = utc_now()
        self.started = time.monotonic()
        self.lock = threading.Lock()

    def emit(self, status, detail=''):
        message = (
            f'[{self.label}|{status}] {utc_now()} elapsed={time.monotonic() - self.started:.3f}s {detail}'.rstrip()
        )
        with self.lock:
            print(message, flush=True)
            with self.log.open('a') as stream:
                stream.write(message + '\n')

    def execute(self, command, *, env=None, cwd=None):
        command = [str(item) for item in command]
        require(not self.record_path.exists(), f'preserve the existing attempt: {self.directory}')
        self.emit('start', shlex.join(command))
        process = subprocess.Popen(
            command, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True
        )
        heartbeat = time.monotonic() + 30
        assert process.stdout is not None
        try:
            with process.stdout, selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    for key, _ in selector.select(timeout=0.2):
                        chunk = os.read(key.fd, 65536)
                        if chunk:
                            with self.log.open('ab') as stream:
                                stream.write(chunk)
                        else:
                            selector.unregister(key.fileobj)
                    if time.monotonic() >= heartbeat:
                        self.emit('running', f'pid={process.pid}')
                        heartbeat = time.monotonic() + 30
            result = process.wait()
        except BaseException:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            raise
        atomic_json(
            self.record_path,
            {
                'command': command,
                'started_at': self.started_at,
                'ended_at': utc_now(),
                'elapsed_seconds': time.monotonic() - self.started,
                'exit_code': result,
                'log': str(self.log),
            },
        )
        self.emit('success' if result == 0 else 'failed', f'exit={result}')
        if result:
            raise OrchestrationError(f'{self.label} failed with exit {result}; inspect {self.log}')


class ReleaseEnvironment:
    def __init__(self, config):
        self.config = config
        self.pins = pins(node_version=config['node_version'])
        self.tools = tool_roots(config)
        self.workspace = config['workspace']
        self.evidence = config['evidence']
        self.cache = config['cache']
        default_patch = self.evidence / 'environment/stack-b-source.diff'
        if config.get('stack_b_patch') is None and default_patch.is_file():
            self.config['stack_b_patch'] = default_patch

    def source(self, name):
        return self.workspace / (name if name in ('base', 'cvm') else 'cube-' + name[-1])

    def image(self, target):
        return f'{self.config["image_prefix"]}-{target}:ci-jammy-v1'

    def host_preflight(self, kvm=False):
        require(sys.platform == 'linux', 'builds and KVM acceptance require a Linux control host')
        require(os.uname().machine == 'x86_64', 'the release builder requires x86_64 Linux')
        commands = ['docker', 'git', 'python3', 'sha256sum']
        if kvm:
            commands += ['qemu-system-x86_64', 'qemu-img', 'cloud-localds', 'openssl', 'ssh', 'ssh-keygen']
        missing = [name for name in commands if shutil.which(name) is None]
        require(not missing, 'missing control-host commands: ' + ', '.join(missing))
        checked(['docker', 'info', '--format', '{{.ServerVersion}}'], timeout=30)
        require(os.cpu_count() >= self.config['minimum_cpus'], 'insufficient physical CPU threads')
        require(
            shutil.disk_usage(self.workspace.parent).free / 1024**3 >= self.config['minimum_free_gib'],
            'insufficient free disk',
        )
        if kvm:
            require(
                Path('/dev/kvm').is_char_device() and os.access('/dev/kvm', os.R_OK | os.W_OK),
                '/dev/kvm is not accessible',
            )
        return {'platform': 'linux-x86_64', 'cpu_threads': os.cpu_count(), 'kvm': kvm}

    def pinned_base_image(self):
        if run(['docker', 'image', 'inspect', self.config['base_image']]).returncode:
            Runner(self.evidence / 'environment/base-image', 'pull-base-image').execute(
                ['docker', 'pull', self.config['base_image']]
            )
        return (
            checked(['docker', 'image', 'inspect', self.config['base_image'], '--format', '{{index .RepoDigests 0}}'])
            .stdout.decode()
            .strip()
        )

    def prepare(self):
        self.host_preflight()
        for directory in (
            self.workspace,
            self.evidence,
            self.cache,
            self.config['tools_directory'],
            self.cache / 'go',
            self.cache / 'downloads',
        ):
            directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        build_environment = os.environ.copy()
        build_environment['DOCKER_BUILDKIT'] = '1' if run(['docker', 'buildx', 'version']).returncode == 0 else '0'
        self.prepare_sources()
        self.prepare_tools()
        pinned = self.pinned_base_image()
        require('@sha256:' in pinned, 'base_image must resolve to a repository digest')

        def build_image(target):
            Runner(self.evidence / 'environment' / target, 'prepare-image-' + target).execute(
                [
                    'docker',
                    'build',
                    '--file',
                    str(DOCKERFILE),
                    '--target',
                    target,
                    '--build-arg',
                    'BASE_IMAGE=' + pinned,
                    '--tag',
                    self.image(target),
                    str(ROOT),
                ],
                env=build_environment,
            )

        with ThreadPoolExecutor(max_workers=3) as pool:
            wait_all(
                [pool.submit(build_image, target) for target in ('release', 'cube', 'packager')], 'image preparation'
            )
        self.prepare_formatters()
        images = {}
        for target in ('release', 'cube', 'packager'):
            identity = (
                checked(['docker', 'image', 'inspect', self.image(target), '--format', '{{.Id}}'])
                .stdout.decode()
                .strip()
            )
            images[target] = {'reference': self.image(target), 'id': identity}
        atomic_json(
            self.evidence / 'environment/images.json',
            {
                'base_image': pinned,
                'buildkit': build_environment['DOCKER_BUILDKIT'] == '1',
                'images': images,
                'pins': self.pins,
            },
        )

    def prepare_sources(self):
        repository = self.config['source_repository']
        head = (
            checked(['git', '-C', str(repository), 'rev-parse', self.config['revision'] + '^{commit}'])
            .stdout.decode()
            .strip()
        )
        for name in ('base', 'cvm', 'cube-a', 'cube-b'):
            source = self.source(name)
            if not source.exists():
                checked(['git', '-C', str(repository), 'worktree', 'add', '--detach', str(source), head])
            require(
                checked(['git', '-C', str(source), 'rev-parse', 'HEAD']).stdout.decode().strip() == head,
                f'{name} source is not the selected revision',
            )
        patch = self.config.get('stack_b_patch')
        if patch is None:
            directory = self.evidence / 'environment/stack-b-patch'
            directory.mkdir(parents=True, exist_ok=True)
            patch = directory / 'stack-b.patch'
            changes = []
            for relative in (
                'components/cube/CubeShim/shim/build.rs',
                'components/cube/Cubelet/pkg/version/version.go',
            ):
                before = (self.source('cube-a') / relative).read_text()
                require(before.count('0.0.0-dev') == 1, f'cannot derive a minimal Stack B patch for {relative}')
                replacement = directory / Path(relative).name
                replacement.write_text(before.replace('0.0.0-dev', '0.0.0-b'))
                result = run(
                    [
                        'diff',
                        '-u',
                        '--label',
                        'a/' + relative,
                        str(self.source('cube-a') / relative),
                        '--label',
                        'b/' + relative,
                        str(replacement),
                    ]
                )
                require(result.returncode == 1, 'Stack B patch generation did not produce a source change')
                changes.append(result.stdout.decode())
            patch.write_text(''.join(changes))
        b = self.source('cube-b')
        if not run(['git', '-C', str(b), 'status', '--porcelain']).stdout:
            checked(['git', '-C', str(b), 'apply', str(patch)])
        checked(['git', '-C', str(b), 'apply', '--check', '--reverse', str(patch)])
        canonical = self.evidence / 'environment/stack-b-source.diff'
        canonical.parent.mkdir(parents=True, exist_ok=True)
        content = checked(['git', '-C', str(b), 'diff', '--no-ext-diff', '--binary', 'HEAD']).stdout
        require(
            b'0.0.0-b' in content if patch == self.evidence / 'environment/stack-b-patch/stack-b.patch' else content,
            'empty Stack B source patch',
        )
        temporary = canonical.with_suffix('.tmp')
        temporary.write_bytes(content)
        temporary.chmod(0o600)
        temporary.replace(canonical)
        self.config['stack_b_patch'] = canonical

    def prepare_tools(self):
        if not self.config['install_missing_tools']:
            for name, path in self.tools.items():
                require(path.is_dir(), f'missing frozen {name} toolchain: {path}')
            return
        root = self.config['tools_directory']
        if not (self.tools['cargo'] / 'bin/rustup').is_file():
            install = root / 'rustup-init'
            checked(
                [
                    'curl',
                    '--fail',
                    '--silent',
                    '--show-error',
                    '--location',
                    'https://sh.rustup.rs',
                    '--output',
                    str(install),
                ]
            )
            checked(
                ['sh', str(install), '-y', '--profile', 'minimal', '--default-toolchain', 'none', '--no-modify-path'],
                env={**os.environ, 'CARGO_HOME': str(self.tools['cargo']), 'RUSTUP_HOME': str(self.tools['rustup'])},
            )
            install.unlink()
        env = {**os.environ, 'CARGO_HOME': str(self.tools['cargo']), 'RUSTUP_HOME': str(self.tools['rustup'])}
        checked(
            [
                str(self.tools['cargo'] / 'bin/rustup'),
                'toolchain',
                'install',
                self.pins['rust'],
                '--profile',
                'minimal',
                '--component',
                'rustfmt,clippy,rust-src',
                '--target',
                'x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu,x86_64-unknown-linux-musl,aarch64-unknown-linux-musl',
            ],
            env=env,
        )
        downloads = {
            'node': (
                f'https://nodejs.org/dist/v{self.pins["node"]}/node-v{self.pins["node"]}-linux-x64.tar.xz',
                'node-v' + self.pins['node'] + '-linux-x64',
            ),
            'go': (f'https://go.dev/dl/go{self.pins["go"]}.linux-amd64.tar.gz', 'go'),
            'zig': (
                f'https://ziglang.org/download/{self.pins["zig_version"]}/zig-x86_64-linux-{self.pins["zig_version"]}.tar.xz',
                'zig-x86_64-linux-' + self.pins['zig_version'],
            ),
        }
        for name, (url, directory) in downloads.items():
            if self.tools[name].is_dir():
                continue
            archive = root / Path(url).name
            checked(['curl', '--fail', '--silent', '--show-error', '--location', url, '--output', str(archive)])
            if name == 'zig':
                expected = self.pins['zig_sha256']
            else:
                expected = self.config['tool_sha256'][name]
            require(digest(archive) == expected, f'downloaded {name} does not match its explicit SHA-256')
            temporary = root / ('.extract-' + name)
            require(not temporary.exists(), 'preserve an interrupted tool extraction for inspection')
            temporary.mkdir(mode=0o700)
            checked(['tar', '-xf', str(archive), '-C', str(temporary)])
            checked(['mv', str(temporary / directory), str(self.tools[name])])
            temporary.rmdir()
            archive.unlink()

    def prepare_formatters(self):
        source = self.source('base')
        command = self.container(f'brocade-format-tools-{os.getpid()}', 'release', source, entrypoint=True)
        command[command.index('--workdir') : command.index('--workdir') + 2] = []
        Runner(self.evidence / 'environment/formatters', 'prepare-formatters').execute(
            command + ['-c', 'cd "$1" && python3 scripts/format.py --install', 'sh', str(source)]
        )
        for name in ('cvm', 'cube-a', 'cube-b'):
            target = self.source(name) / '.tools'
            target.mkdir(exist_ok=True)
            if not (target / 'format-tools').exists():
                shutil.copytree(source / '.tools/format-tools', target / 'format-tools', symlinks=True)

    def require_tools(self):
        for name, path in self.tools.items():
            require(path.is_dir(), f'missing frozen {name} toolchain: {path}')
        for name, path in self.config['inputs'].items():
            require(path.exists(), f'missing configured input {name}: {path}')
        heads = {}
        for name in ('base', 'cvm', 'cube-a', 'cube-b'):
            source = self.source(name)
            require((source / 'scripts/build-release.py').is_file(), f'missing independent source: {source}')
            top = checked(['git', '-C', str(source), 'rev-parse', '--show-toplevel']).stdout.decode().strip()
            require(Path(top).resolve() == source, f'Git metadata is incomplete for {name}')
            heads[name] = checked(['git', '-C', str(source), 'rev-parse', 'HEAD']).stdout.decode().strip()
            self.git_common(source)
        for name in ('base', 'cvm', 'cube-a'):
            require(
                not run(['git', '-C', str(self.source(name)), 'status', '--porcelain']).stdout,
                f'unexpected source changes in {name}',
            )
        patch = self.config.get('stack_b_patch')
        require(patch is not None and patch.is_file(), 'Stack B canonical source patch is missing; run prepare first')
        b = self.source('cube-b')
        checked(['git', '-C', str(b), 'apply', '--check', '--reverse', str(patch)])
        actual = checked(['git', '-C', str(b), 'diff', '--no-ext-diff', '--binary', 'HEAD']).stdout
        require(actual == patch.read_bytes(), 'Stack B working tree differs from its canonical source patch')
        require(len(set(heads.values())) == 1, 'all four build sources must have the same Git HEAD')
        return heads['base']

    def git_common(self, source):
        value = (
            checked(['git', '-C', str(source), 'rev-parse', '--path-format=absolute', '--git-common-dir'])
            .stdout.decode()
            .strip()
        )
        common = Path(value).resolve()
        require(common.is_dir(), f'Git common directory is unavailable: {common}')
        return common

    def mount_git(self, source):
        common = self.git_common(source)
        return ['--volume', f'{common}:{common}:ro']

    def container(self, name, target, source, entrypoint=False, extra_env=None):
        environment = {
            'HOME': '/home/build',
            'CARGO_HOME': '/home/build/cargo',
            'RUSTUP_HOME': '/home/build/rustup',
            'GOPATH': '/home/build/go',
            'GOCACHE': '/home/build/cache/go',
            'GOMODCACHE': '/home/build/go/pkg/mod',
            'ZIG_GLOBAL_CACHE_DIR': '/home/build/cache/zig',
            'npm_config_cache': '/home/build/cache/npm',
            'BROCADE_GO': '/home/build/tool-go/bin/go',
            'BROCADE_ZIG': '/home/build/tool-zig/zig',
            'RUSTUP_TOOLCHAIN': self.pins['rust'],
            'PATH': '/home/build/tool-node/bin:/home/build/tool-go/bin:/home/build/tool-zig:/home/build/cargo/bin:/usr/lib/llvm-21/bin:/usr/local/bin:/usr/bin:/bin',
        }
        environment.update(extra_env or {})
        command = ['docker', 'run', '--rm', '--name', name, '--init', '--user', f'{os.getuid()}:{os.getgid()}']
        for key, value in environment.items():
            command += ['--env', f'{key}={value}']
        command += [
            '--volume',
            f'{source}:{source}',
            *self.mount_git(source),
        ]
        for other in ('base', 'cvm', 'cube-a', 'cube-b'):
            candidate = self.source(other)
            if candidate != source:
                command += ['--volume', f'{candidate}:{candidate}', *self.mount_git(candidate)]
        command += [
            '--volume',
            f'{self.tools["cargo"]}:/home/build/cargo',
            '--volume',
            f'{self.tools["rustup"]}:/home/build/rustup',
            '--volume',
            f'{self.tools["node"]}:/home/build/tool-node:ro',
            '--volume',
            f'{self.tools["go"]}:/home/build/tool-go:ro',
            '--volume',
            f'{self.tools["zig"]}:/home/build/tool-zig:ro',
            '--volume',
            f'{self.cache / "go"}:/home/build/go',
            '--volume',
            f'{self.cache / "downloads"}:/home/build/cache',
        ]
        for path in self.config['inputs'].values():
            command += ['--volume', f'{path}:{path}:ro']
        if entrypoint:
            command += ['--entrypoint', '/bin/sh']
        return command + ['--workdir', str(source), self.image(target)]

    def check_target(self, name):
        marker = self.source(name) / 'target/.brocade-build-environment.json'
        target = 'cube' if name.startswith('cube') else 'release'
        identity = (
            checked(['docker', 'image', 'inspect', self.image(target), '--format', '{{.Id}}']).stdout.decode().strip()
        )
        expected = {'version': 1, 'image': identity}
        if marker.exists():
            previous = json.loads(marker.read_text())
            if previous != expected:
                # These caches contain host-executed build scripts. Preserve a manifest and
                # refuse the run instead of linking them under another libc or compiler image.
                raise OrchestrationError(f'{name} target belongs to another build environment: {marker}')
        else:
            compiled = [self.source(name) / 'target/brocade-agent-build', self.source(name) / 'target/brocade-release']
            if name.startswith('cube'):
                compiled.append(self.source(name) / 'target/cube')
            require(
                not any(path.exists() for path in compiled),
                f'{name} has unowned compiled caches; refusing to mix them with CI',
            )
            atomic_json(marker, expected)

    def clear_owned_targets(self):
        """Explicitly retire only caches carrying this orchestrator's environment marker."""
        for name in ('base', 'cvm', 'cube-a', 'cube-b'):
            target = self.source(name) / 'target'
            marker = target / '.brocade-build-environment.json'
            if not marker.exists():
                continue
            record = json.loads(marker.read_text())
            require(
                record.get('version') == 1 and isinstance(record.get('image'), str), f'invalid cache identity: {marker}'
            )
            archive = self.evidence / 'retired-targets' / (name + '-' + str(int(time.time())) + '.json')
            atomic_json(archive, {'source': str(target), 'identity': record})
            for child in target.iterdir():
                if child.is_dir() and not child.is_symlink():
                    shutil.rmtree(child)
                else:
                    child.unlink()

    def probe(self, name):
        source = self.source(name)
        target = 'cube' if name.startswith('cube') else 'release'
        self.check_target(name)
        script = """set -eu
node --version
npm --version
test "$(node --version)" = "v$NODE_VERSION"
go version
test "$(go version)" = "go version go$GO_VERSION linux/amd64"
zig version
test "$(zig version)" = "$ZIG_VERSION"
cargo --version
rustc --version
rustc --version | grep -F "rustc $RUST_VERSION "
if test "$TARGET_KIND" = cube; then clang-21 --version; llvm-objcopy-21 --version; debugfs -V; fi
probe=$(mktemp -d /home/build/cache/compiler-probe.XXXXXX)
trap 'rm -rf "$probe"' EXIT
printf 'int main(void) { return 0; }\n' > "$probe/probe.c"
cc "$probe/probe.c" -o "$probe/probe"
"$probe/probe"
printf 'fn main() { assert_eq!(2 + 2, 4); }\n' > "$probe/main.rs"
rustc "$probe/main.rs" -o "$probe/rust-probe"
"$probe/rust-probe"
python3 -c 'import sys; assert sys.version_info >= (3, 10)'
"""
        command = self.container(
            f'brocade-preflight-{os.getpid()}-{name}',
            target,
            source,
            entrypoint=True,
            extra_env={
                'TARGET_KIND': target,
                'NODE_VERSION': self.pins['node'],
                'GO_VERSION': self.pins['go'],
                'ZIG_VERSION': self.pins['zig_version'],
                'RUST_VERSION': self.pins['rust'],
            },
        )
        command[command.index('--workdir') : command.index('--workdir') + 2] = []
        self.phase(self.evidence / 'preflight', name, command, retryable=True)

    def preflight(self, kvm=False):
        host = self.host_preflight(kvm=kvm)
        head = self.require_tools()
        report_path = self.evidence / 'preflight/report.json'
        report = {'status': 'running', 'host': host, 'head': head, 'pins': self.pins, 'checked_at': utc_now()}
        atomic_json(report_path, report)
        try:
            with ThreadPoolExecutor(max_workers=4) as pool:
                wait_all(
                    [pool.submit(self.probe, name) for name in ('base', 'cvm', 'cube-a', 'cube-b')],
                    'compiler preflight',
                )
        except BaseException as error:
            report.update(status='failed', error=str(error), checked_at=utc_now())
            atomic_json(report_path, report)
            raise
        report.update(status='passed', checked_at=utc_now())
        atomic_json(report_path, report)
        return report

    def build_one(self, name):
        self.check_target(name)
        args = ['python3', 'scripts/build-release.py', '--jobs', str(self.config['jobs'][name])]
        if name in ('base', 'cvm'):
            args += ['--flavor', name]
        else:
            args += [
                '--cube-runtime-only',
                '--cube-arch',
                'x86_64',
                '--cube-glibc',
                '2.35',
                '--cube-sysroot-libs',
                str(self.config['inputs']['cube_sysroot_libs']),
                '--cube-bpf-include',
                str(self.config['inputs']['cube_bpf_include']),
            ]
            if self.config['inputs'].get('cube_assets'):
                args += ['--cvm-package-assets', str(self.config['inputs']['cube_assets'])]
        runner = Runner(self.evidence / 'build' / name, 'build-' + name)
        runner.execute(
            self.container(
                f'brocade-build-{os.getpid()}-{name}',
                'cube' if name.startswith('cube') else 'release',
                self.source(name),
            )
            + args
        )

    def build_all(self):
        self.preflight()
        with ThreadPoolExecutor(max_workers=4) as pool:
            futures = {pool.submit(self.build_one, name): name for name in ('base', 'cvm', 'cube-a', 'cube-b')}
            failures = []
            for future in as_completed(futures):
                try:
                    future.result()
                except BaseException as error:
                    failures.append((futures[future], error))
            if failures:
                raise OrchestrationError('; '.join(f'{name}: {error}' for name, error in failures))

    def verify(self):
        head = self.require_tools()
        output = self.evidence / 'candidate/verification.json'
        frozen = self.evidence / 'candidate/deployment-build'
        require(not frozen.exists(), f'preserve the existing frozen candidate: {frozen}')
        runner = Runner(self.evidence / 'candidate', 'verify-candidate')
        runner.execute(
            self.container(
                f'brocade-verify-{os.getpid()}',
                'packager',
                self.source('cube-a'),
                entrypoint=True,
            )
            + [
                '-c',
                'python3 -I "$1/scripts/verify_release_candidate.py" --workspace "$2" --output "$3"',
                'sh',
                str(self.source('cube-a')),
                str(self.workspace),
                str(output),
            ]
        )
        result = json.loads(output.read_text())
        require(result['result'] == 'verified', 'candidate verification did not pass')
        result['head'] = head
        result['deployment_ready'] = False
        atomic_json(output, result)
        self.freeze_candidate(result)
        return result

    def freeze_candidate(self, verification):
        """Copy deployment inputs once; acceptance never reads mutable build caches."""
        frozen = self.evidence / 'candidate/deployment-build'
        require(not frozen.exists(), f'preserve the existing frozen candidate: {frozen}')
        hashes = {}
        files = {}
        source = self.source('cvm')
        for name, path in (
            ('console', source / 'target/brocade-release/cvm/cargo/x86_64-unknown-linux-gnu/release/brocade-console'),
            ('agent', source / 'target/brocade-agent-build/cvm/x86_64-unknown-linux-musl/release/brocade-agent'),
        ):
            relative = (
                Path('target')
                / (
                    'brocade-release/cvm/cargo/x86_64-unknown-linux-gnu/release'
                    if name == 'console'
                    else 'brocade-agent-build/cvm/x86_64-unknown-linux-musl/release'
                )
                / ('brocade-console' if name == 'console' else 'brocade-agent')
            )
            files[relative] = path
            hashes[str(relative)] = digest(path)
        require(
            hashes['target/brocade-release/cvm/cargo/x86_64-unknown-linux-gnu/release/brocade-console']
            == verification['flavors']['cvm']['artifacts']['x86_64-unknown-linux-gnu/brocade-console']['sha256'],
            'frozen Console differs from verification',
        )
        require(
            hashes['target/brocade-agent-build/cvm/x86_64-unknown-linux-musl/release/brocade-agent']
            == verification['flavors']['cvm']['agents']['cvm/x86_64']['sha256'],
            'frozen Agent differs from verification',
        )
        for stack in ('a', 'b'):
            item = verification['stacks'][stack]
            bundle = Path(item['bundle'])
            require(digest(bundle) == item['sha256'], 'bundle changed before freezing')
            relative = Path('frozen') / ('stack-' + stack + '.tar.gz')
            files[relative] = bundle
            hashes[str(relative)] = item['sha256']
        script = self.source('cube-a') / 'scripts/cvm_stack.py'
        files[Path('scripts/cvm_stack.py')] = script
        hashes['scripts/cvm_stack.py'] = digest(script)
        for relative, path in files.items():
            target = frozen / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
            target.chmod(0o600)
            require(digest(target) == hashes[str(relative)], 'candidate changed while freezing')
        a = verification['stacks']['a']
        receipt = {
            'bundle': str(frozen / 'frozen/stack-a.tar.gz'),
            'sha256': a['sha256'],
            'architecture': 'x86_64',
            'deployment_ready': False,
        }
        bundle_dir = frozen / 'target/cube/artifacts/x86_64'
        bundle_dir.mkdir(parents=True)
        atomic_json(bundle_dir / 'bundle.json', receipt)
        atomic_json(
            frozen / 'input-hashes.json', {'head': verification['head'], 'files': hashes, 'deployment_ready': False}
        )
        return frozen

    def kvm_inputs(self):
        inputs = self.config['inputs']
        for key in ('os_image', 'cube_assets'):
            require(inputs.get(key) is not None, f'KVM acceptance requires inputs.{key}')
        image = inputs['os_image']
        require(image.is_file() and not image.is_symlink(), 'OS image must be a regular file')
        require(
            self.config.get('os_image_sha256') and digest(image) == self.config['os_image_sha256'],
            'OS image differs from its explicit SHA-256',
        )
        for key, sha_key in (('geoip', 'geoip_sha256'), ('geosite', 'geosite_sha256')):
            require(
                inputs.get(key) is not None and inputs[key].is_file(), f'KVM acceptance requires pinned inputs.{key}'
            )
            require(
                self.config.get(sha_key) and digest(inputs[key]) == self.config[sha_key],
                f'{key} differs from its explicit SHA-256',
            )
        total = int(re.search(r'MemTotal:\s+(\d+) kB', Path('/proc/meminfo').read_text()).group(1)) / 1024**2
        require(
            self.config['kvm_memory_gib'] + self.config['reserved_memory_gib'] <= total,
            'KVM memory reservation exceeds physical memory',
        )

    def accept(self):
        self.host_preflight(kvm=True)
        self.kvm_inputs()
        verification_path = self.evidence / 'candidate/verification.json'
        require(verification_path.is_file(), 'run verify before KVM acceptance')
        verification = json.loads(verification_path.read_text())
        frozen = self.evidence / 'candidate/deployment-build'
        require(frozen.is_dir(), 'frozen deployment inputs are missing')
        identity = json.loads((frozen / 'input-hashes.json').read_text())
        require(identity['head'] == verification['head'], 'frozen candidate does not match verification')
        for name, expected in identity['files'].items():
            require(digest(frozen / name) == expected, f'frozen deployment input changed: {name}')
        run_root = self.config['run_directory']
        run_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        selector = self.config['acceptance_selector']
        selected = ('main', 'stack', 'first-install') if selector == 'all' else (selector,)
        cvm = self.source('cube-a') / 'scripts/tests/cvm'
        for name in selected:
            require(cvm.is_dir(), 'acceptance source is missing')
        jobs = []
        if 'stack' in selected or 'first-install' in selected:
            catalog = run_root / 'stack/catalog'
            if not (catalog / 'lab-catalog.json').exists():
                a, b = verification['stacks']['a'], verification['stacks']['b']
                self.phase(
                    run_root,
                    'stack-catalog',
                    [
                        sys.executable,
                        str(cvm / 'release_stack_acceptance.py'),
                        'catalog',
                        '--output',
                        str(catalog),
                        '--source',
                        str(frozen / 'frozen/stack-a.tar.gz'),
                        '--source-sha256',
                        a['sha256'],
                        '--target',
                        str(frozen / 'frozen/stack-b.tar.gz'),
                        '--target-sha256',
                        b['sha256'],
                    ],
                )
            if 'stack' in selected:
                jobs.append(('stack', run_root / 'stack', False))
        if 'first-install' in selected:
            require(
                (run_root / 'stack/catalog/lab-catalog.json').is_file(),
                'first-install requires the signed Stack catalog',
            )
            jobs.append(('first-install', run_root / 'first-install', True))
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(self.prepare_lab, name, root, cvm, frozen) for name, root, _ in jobs]
            wait_all(futures, 'lab preparation')
        if 'main' in selected:
            self.main_acceptance(run_root / 'main', cvm, frozen)
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = []
            for name, root, fresh in jobs:
                if fresh:
                    futures.append(pool.submit(self.first_install, root, cvm))
                else:
                    futures.append(pool.submit(self.stack_acceptance, root, cvm))
            wait_all(futures, 'KVM acceptance')
        atomic_json(
            run_root / 'acceptance.json',
            {'result': 'passed', 'head': verification['head'], 'suites': selected, 'completed_at': utc_now()},
        )

    def phase(self, root, name, command, *, retryable=False):
        directory = root / 'steps' / name
        record = directory / 'result.json'
        if record.exists():
            previous = json.loads(record.read_text())
            if previous['exit_code'] == 0:
                return False
            require(
                retryable,
                f'failed mutating phase {name} is preserved; use a new disposable run after diagnosis: {directory}',
            )
            attempt = 2
            while (root / 'steps' / f'{name}-attempt-{attempt}').exists():
                attempt += 1
            directory.rename(root / 'steps' / f'{name}-attempt-{attempt}')
        require(
            not (directory / 'execution.log').exists() or retryable,
            f'incomplete phase {name} is preserved; use a new disposable run: {directory}',
        )
        Runner(directory, root.name + '-' + name).execute(command)
        return True

    def prepare_lab(self, name, root, cvm, frozen):
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        lab = root / 'lab-3'
        if lab.exists():
            require((lab / 'lab.json').is_file(), f'incomplete lab is preserved; use a new run directory: {lab}')
        if not (lab / 'lab.json').exists():
            self.phase(
                root,
                'lab-prepare',
                [
                    sys.executable,
                    str(cvm / 'release_kvm_lab.py'),
                    'prepare',
                    '--output',
                    str(lab),
                    '--image',
                    str(self.config['inputs']['os_image']),
                    '--image-sha256',
                    self.config['os_image_sha256'],
                ],
            )
        if not (root / 'prerequisites/prerequisites.json').exists():
            self.phase(
                root,
                'prerequisites',
                [
                    sys.executable,
                    str(cvm / 'release_kvm_services.py'),
                    '--lab',
                    str(lab),
                    '--output',
                    str(root / 'prerequisites'),
                ],
            )
        inputs = root / 'services/inputs'
        inputs.mkdir(mode=0o700, parents=True, exist_ok=True)
        for key in ('geoip', 'geosite'):
            target = inputs / (key + '.dat')
            if not target.exists():
                shutil.copyfile(self.config['inputs'][key], target)
                target.chmod(0o600)
            require(digest(target) == self.config[key + '_sha256'], f'frozen {key} asset differs from its pin')
        if not (root / 'services/input-hashes.json').exists():
            command = [sys.executable, str(cvm / 'release_kvm_deploy.py')]
            if name == 'first-install':
                command.append('--agent-only-compute')
            command += [str(lab), str(frozen), str(root / 'services')]
            self.phase(root, 'deployment', command)
        self.phase(
            root,
            'verify-owned',
            [
                sys.executable,
                '-I',
                '-c',
                'import sys;sys.path.insert(0,sys.argv[1]);from release_kvm_lab import Lab;Lab(sys.argv[2]).verify_owned()',
                str(cvm),
                str(root / 'lab-3'),
            ],
            retryable=True,
        )

    def main_acceptance(self, root, cvm, frozen):
        self.prepare_lab('main', root, cvm, frozen)
        for phase in MAIN_PHASES:
            self.phase(root, phase, [sys.executable, str(cvm / 'release_kvm_acceptance.py'), str(root), phase])
        if self.config['stop_labs']:
            self.phase(
                root,
                'stop',
                [sys.executable, str(cvm / 'release_kvm_lab.py'), 'stop', '--output', str(root / 'lab-3')],
                retryable=True,
            )

    def stack_acceptance(self, root, cvm):
        state_path = root / 'stack-acceptance/state.json'
        if not state_path.exists():
            self.phase(
                root, 'bootstrap', [sys.executable, str(cvm / 'release_kvm_acceptance.py'), str(root), 'bootstrap']
            )
            self.phase(
                root,
                'stack-prepare',
                [
                    sys.executable,
                    str(cvm / 'release_stack_acceptance.py'),
                    'prepare',
                    '--lab-output',
                    str(root),
                    '--catalog',
                    str(root / 'catalog'),
                    '--active',
                    'alpha',
                    '--idle',
                    'stack-idle',
                    '--seed-missing',
                ],
            )
        state = json.loads(state_path.read_text())
        completed = {name for name, case in state['cases'].items() if case.get('verified') is True}
        incomplete = set(state['cases']) - completed
        require(
            not incomplete,
            'preserve interrupted Stack scenarios for diagnosis; do not rerun their mutating steps: '
            + ', '.join(sorted(incomplete)),
        )
        require(completed <= set(STACK_SCENARIOS), 'unexpected Stack scenario evidence')
        for name in STACK_SCENARIOS:
            if name in completed:
                continue
            self.phase(
                root,
                'stack-' + name,
                [
                    sys.executable,
                    str(cvm / 'release_stack_acceptance.py'),
                    'run',
                    '--lab-output',
                    str(root),
                    '--late-receipt',
                    '--case',
                    name,
                ],
            )
        state = json.loads(state_path.read_text())
        require(
            set(state['cases']) == set(STACK_SCENARIOS)
            and all(case.get('verified') is True for case in state['cases'].values()),
            'not all seven Stack scenarios passed',
        )
        if self.config['stop_labs']:
            self.phase(
                root,
                'stop',
                [sys.executable, str(cvm / 'release_kvm_lab.py'), 'stop', '--output', str(root / 'lab-3')],
                retryable=True,
            )

    def first_install(self, root, cvm):
        evidence = root / 'stack-acceptance/first-install.json'
        if evidence.exists():
            require(
                'installation' in json.loads(evidence.read_text()),
                f'incomplete first-install evidence is preserved; use a new disposable lab: {evidence}',
            )
        if not evidence.exists():
            self.phase(
                root, 'bootstrap', [sys.executable, str(cvm / 'release_kvm_acceptance.py'), str(root), 'bootstrap']
            )
            self.phase(
                root,
                'first-install',
                [
                    sys.executable,
                    str(cvm / 'release_stack_acceptance.py'),
                    'first-install',
                    '--lab-output',
                    str(root),
                    '--catalog',
                    str(self.config['run_directory'] / 'stack/catalog'),
                ],
            )
        record = json.loads(evidence.read_text())
        require(
            record['installation']['previous'] is None and record['installation']['action'] == 'stack-install-activate',
            'first-install evidence is incomplete',
        )
        require(
            record['guest']['root'] and record['claim'] and record['download_fault'],
            'first-install did not verify its required gates',
        )
        if self.config['stop_labs']:
            self.phase(
                root,
                'stop',
                [sys.executable, str(cvm / 'release_kvm_lab.py'), 'stop', '--output', str(root / 'lab-3')],
                retryable=True,
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        'action', choices=['prepare', 'preflight', 'build', 'verify', 'accept', 'status', 'retire-targets']
    )
    parser.add_argument('--environment', type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    environment = ReleaseEnvironment(read_config(args.environment))
    environment.evidence.mkdir(mode=0o700, parents=True, exist_ok=True)
    lock = (environment.evidence / '.orchestrator.lock').open('a')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise OrchestrationError(f'another orchestrator owns this environment: {environment.evidence}') from None
    with lock:
        if args.action == 'prepare':
            environment.prepare()
        elif args.action == 'preflight':
            environment.preflight(kvm=bool(environment.config['inputs'].get('os_image')))
        elif args.action == 'build':
            environment.build_all()
        elif args.action == 'verify':
            environment.verify()
        elif args.action == 'accept':
            environment.accept()
        elif args.action == 'retire-targets':
            environment.clear_owned_targets()
        elif args.action == 'status':
            print(
                json.dumps(
                    {
                        'workspace': str(environment.workspace),
                        'evidence': str(environment.evidence),
                        'jobs': environment.config['jobs'],
                    },
                    indent=2,
                )
            )
        else:
            parser.error(f'{args.action} requires frozen artifacts; see scripts/release-environment/README.md')


if __name__ == '__main__':
    try:
        main()
    except (OrchestrationError, OSError, ValueError, subprocess.SubprocessError) as error:
        print(f'release orchestration stopped: {error}', file=sys.stderr)
        raise SystemExit(1)
