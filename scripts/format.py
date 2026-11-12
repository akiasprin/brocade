#!/usr/bin/env python3
"""Format explicitly selected files or Git changes without traversing vendored trees."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
TOOLS = ROOT / '.tools/format-tools'
PRETTIER = {
    '.ts',
    '.tsx',
    '.js',
    '.jsx',
    '.mjs',
    '.cjs',
    '.json',
    '.jsonc',
    '.css',
    '.scss',
    '.less',
    '.html',
    '.htm',
    '.vue',
    '.yaml',
    '.yml',
    '.md',
    '.mdx',
}
BINARY = {'.png', '.jpg', '.jpeg', '.gif', '.ico', '.webp', '.woff', '.woff2', '.der', '.o', '.gz', '.zip', '.dat'}


def run(command, *, env=None, capture=False):
    if capture:
        return subprocess.check_output(command, cwd=ROOT, env=env)
    started = time.monotonic()
    with subprocess.Popen(command, cwd=ROOT, env=env) as process:
        while True:
            try:
                code = process.wait(timeout=30)
                if code:
                    raise subprocess.CalledProcessError(code, command)
                return
            except subprocess.TimeoutExpired:
                print(f'format: {Path(command[0]).name} running, elapsed {time.monotonic() - started:.1f}s', flush=True)


def git_paths(base):
    revision = base or 'HEAD'
    run(['git', 'rev-parse', '--verify', revision + '^{commit}'], capture=True)
    changed = run(['git', 'diff', '--name-only', '-z', '--diff-filter=ACMR', revision, '--'], capture=True)
    untracked = run(['git', 'ls-files', '--others', '--exclude-standard', '-z'], capture=True)
    return [os.fsdecode(path) for path in (changed + untracked).split(b'\0') if path]


def select_files(paths):
    selected = []
    for value in sorted(set(paths)):
        path = ROOT / value
        if not path.exists():
            raise ValueError(f'format input no longer exists: {value}')
        if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(ROOT):
            raise ValueError(f'format expects a regular file inside the checkout: {value}')
        relative = path.resolve().relative_to(ROOT)
        ignored = subprocess.run(['git', 'check-ignore', '--no-index', '--quiet', '--', str(relative)], cwd=ROOT)
        if ignored.returncode == 0:
            raise ValueError(f'format refuses an ignored local artifact: {value}')
        if ignored.returncode != 1:
            raise ValueError(f'cannot determine formatting scope: {value}')
        if (
            path.suffix.lower() in BINARY
            or path.name.endswith('.lock')
            or path.name in {'package-lock.json', 'npm-shrinkwrap.json', 'pnpm-lock.yaml'}
        ):
            continue
        parts = relative.parts
        if (
            'golden' in parts
            or ('migrations' in parts and path.suffix == '.sql')
            or ('fixtures' in parts and path.suffix == '.sql')
        ):
            print(f'format: preserve immutable SQL/golden fixture {relative}', flush=True)
            continue
        selected.append(relative)
    return selected


def family(path):
    suffix = path.suffix.lower()
    if suffix == '.rs':
        return 'rust'
    if suffix == '.go':
        return 'go'
    if suffix in {'.py', '.pyi'}:
        return 'python'
    if suffix in PRETTIER:
        return 'prettier'
    if suffix == '.svg':
        return 'svg'
    first = (ROOT / path).read_bytes().split(b'\n', 1)[0]
    if suffix == '.sh' or (
        first.startswith(b'#!') and any(shell in first for shell in [b'/sh', b'/bash', b'env bash', b'env sh'])
    ):
        return 'shell'
    return 'text'


def basic_text(path, check):
    original = (ROOT / path).read_bytes()
    if b'\0' in original:
        return
    text = original.decode('utf-8')
    formatted = '\n'.join(line.rstrip(' \t') for line in text.replace('\r\n', '\n').splitlines())
    if text:
        formatted += '\n'
    if formatted.encode() != original:
        if check:
            raise ValueError(f'UTF-8/LF/trailing whitespace/final newline check failed: {path}')
        (ROOT / path).write_text(formatted)


def pinned_tool(name, install):
    versions = json.loads((ROOT / 'format-tools.json').read_text())
    version = versions[name]
    local = TOOLS / ('ruff/bin/ruff' if name == 'ruff' else 'bin/shfmt')
    tool = str(local) if local.is_file() else shutil.which(name)
    flag = '--version' if name == 'ruff' else '-version'
    if tool and run([tool, flag], capture=True).decode().strip().split()[-1] == version:
        return tool
    if not install:
        raise ValueError(f'{name} {version} is required; rerun scripts/format.py with --install')
    TOOLS.mkdir(parents=True, exist_ok=True)
    if name == 'ruff':
        run(
            [
                sys.executable,
                '-m',
                'pip',
                'install',
                '--disable-pip-version-check',
                '--no-deps',
                '--target',
                str(TOOLS / 'ruff'),
                '--upgrade',
                'ruff==' + version,
            ]
        )
    else:
        env = go_env()
        env['GOBIN'] = str(TOOLS / 'bin')
        run(['go', 'install', 'mvdan.cc/sh/v3/cmd/shfmt@' + version], env=env)
    if run([str(local), flag], capture=True).decode().strip().split()[-1] != version:
        raise ValueError(f'installed {name} does not match {version}')
    return str(local)


def go_env():
    env = os.environ.copy()
    env['GOTOOLCHAIN'] = 'go' + (ROOT / 'components/xray-core/.go-version').read_text().strip()
    return env


def prettier(install):
    tool = ROOT / 'frontend/node_modules/.bin/prettier'
    expected = json.loads((ROOT / 'frontend/package-lock.json').read_text())['packages']['node_modules/prettier'][
        'version'
    ]

    def matches():
        return tool.is_file() and run([str(tool), '--version'], capture=True).decode().strip() == expected

    if not matches() and install:
        run(['npm', 'ci', '--prefix', 'frontend', '--ignore-scripts', '--no-audit', '--no-fund'])
    if not matches():
        raise ValueError(f'Prettier {expected} is required; run npm ci --prefix frontend or pass --install')
    return str(tool)


def format_files(files, check, install):
    groups = {}
    for path in files:
        groups.setdefault(family(path), []).append(path)
    for kind, paths in groups.items():
        arguments = [str(ROOT / path) for path in paths]
        print(f'format: {kind}, {len(paths)} files, {"check" if check else "write"}', flush=True)
        if kind == 'rust':
            command = ['rustfmt', '--config', 'skip_children=true']
            if check:
                command.append('--check')
            run(command + arguments)
        elif kind == 'go':
            location = run(['go', 'env', 'GOROOT'], env=go_env(), capture=True).decode().strip()
            command = [str(Path(location) / 'bin/gofmt')]
            if check:
                result = run(command + ['-l'] + arguments, capture=True).decode().strip()
                if result:
                    raise ValueError('gofmt changes required:\n' + result)
            else:
                run(command + ['-w'] + arguments)
        elif kind == 'python':
            run([pinned_tool('ruff', install), 'format'] + (['--check'] if check else []) + arguments)
        elif kind == 'shell':
            run([pinned_tool('shfmt', install), '-i', '2', '-d' if check else '-w'] + arguments)
        elif kind in {'prettier', 'svg'}:
            parser = ['--parser', 'html'] if kind == 'svg' else []
            run([prettier(install), '--check' if check else '--write'] + parser + ['--'] + arguments)
        else:
            for path in paths:
                basic_text(path, check)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='read-only validation')
    parser.add_argument('--base', help='also include committed changes since this revision')
    parser.add_argument('--install', action='store_true', help='install missing pinned formatters into .tools')
    parser.add_argument('paths', nargs='*', help='explicit files; defaults to changed and untracked Git files')
    args = parser.parse_args()
    try:
        if args.install:
            pinned_tool('ruff', True)
            pinned_tool('shfmt', True)
            prettier(True)
        files = select_files(args.paths or git_paths(args.base))
        if not files:
            print('format: no applicable changed files')
            return 0
        format_files(files, args.check, args.install)
        if not args.check:
            format_files(files, True, False)
    except (ValueError, OSError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f'format failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
