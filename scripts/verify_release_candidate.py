#!/usr/bin/env python3
"""Verify frozen release candidate identities and their glibc/ELF boundaries."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
from cvm_elf import validate as validate_cube_elf
import cvm_package
import cvm_stack


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def console_elf(path, architecture):
    header = subprocess.check_output(['readelf', '-h', str(path)], text=True)
    machine = 'Advanced Micro Devices X86-64' if architecture == 'x86_64' else 'AArch64'
    if not re.search(r'^\s*Machine:\s+' + re.escape(machine) + r'\s*$', header, re.MULTILINE):
        raise ValueError(f'wrong Console architecture: {path}')
    dynamic = subprocess.check_output(['readelf', '-d', str(path)], text=True)
    allowed = {
        'libc.so.6',
        'libm.so.6',
        'libgcc_s.so.1',
        'liblzma.so.5',
        'ld-linux-x86-64.so.2',
        'ld-linux-aarch64.so.1',
    }
    required = set(re.findall(r'Shared library: \[([^]]+)\]', dynamic))
    if not required <= allowed:
        raise ValueError(f'unexpected GNU shared library dependencies for {path}: {sorted(required - allowed)}')
    versions = subprocess.check_output(['readelf', '--version-info', '--wide', str(path)], text=True)
    imported = {tuple(map(int, item)) for item in re.findall(r'GLIBC_(\d+)\.(\d+)', versions)}
    if not imported or max(imported) > (2, 35):
        raise ValueError(f'Console exceeds glibc 2.35: {path}')
    return '.'.join(map(str, max(imported)))


def verify(workspace, output, assets):
    guest_assets = json.loads((assets / 'manifest.json').read_text())
    result = {'format_version': 1, 'result': 'verified', 'flavors': {}, 'stacks': {}}
    for flavor in ('base', 'cvm'):
        source = workspace / flavor
        state = source / 'target/brocade-release' / flavor
        preparation = json.loads((state / 'preparation.json').read_text())
        if (
            preparation['status'] != 'built'
            or preparation['build_complete'] is not True
            or preparation['deployment_ready'] is not False
        ):
            raise ValueError(f'{flavor} release build is incomplete')
        if preparation.get('remaining_gates') != [
            'impact-scoped validation',
            'first-release variant installation acceptance',
        ]:
            raise ValueError(f'{flavor} release record has unexpected remaining gates')
        artifacts = {}
        for key, item in preparation['artifacts'].items():
            architecture = key.split('/', 1)[0].split('-', 1)[0]
            path = Path(item['path'])
            if not path.is_file() or path.is_symlink() or digest(path) != item['sha256']:
                raise ValueError(f'Console artifact differs from preparation: {path}')
            glibc = console_elf(path, architecture)
            artifacts[key] = {'sha256': item['sha256'], 'bytes': path.stat().st_size, 'glibc': glibc}
        agents = {}
        variants = ('base', 'cvm') if flavor == 'cvm' else ('base',)
        for variant in variants:
            for architecture in ('x86_64', 'aarch64'):
                agent = (
                    source
                    / f'target/brocade-agent-build/{variant}/{architecture}-unknown-linux-musl/release/brocade-agent'
                )
                dynamic = subprocess.check_output(['readelf', '-d', str(agent)], text=True)
                if 'Shared library:' in dynamic:
                    raise ValueError(f'Agent is not static: {agent}')
                agents[f'{variant}/{architecture}'] = {'sha256': digest(agent), 'bytes': agent.stat().st_size}
        result['flavors'][flavor] = {'artifacts': artifacts, 'agents': agents}
    for stack in ('a', 'b'):
        source = workspace / ('cube-' + stack)
        directory = source / 'target/cube/artifacts/x86_64'
        runtime = json.loads((directory / 'manifest.json').read_text())
        if runtime['result'] != 'build_complete' or runtime['deploymentReady'] is not False:
            raise ValueError(f'Cube {stack} build is incomplete')
        if runtime['tools']['rust'] != '1.96.0' or runtime['tools']['gnuLibcBaseline'] != '2.35':
            raise ValueError(f'Cube {stack} used the wrong release toolchain')
        for name, item in runtime['artifacts'].items():
            path = directory / name
            if digest(path) != item['sha256'] or path.stat().st_size != item['bytes']:
                raise ValueError(f'Cube {stack} artifact differs from its manifest: {name}')
            validate_cube_elf(path, 'x86_64', '2.35')
        receipt = json.loads((directory / 'bundle.json').read_text())
        bundle = Path(receipt['bundle'])
        if digest(bundle) != receipt['sha256'] or receipt['deployment_ready'] is not False:
            raise ValueError(f'Cube {stack} bundle differs from its content identity')
        packaged = cvm_stack.inspect_archive(bundle, receipt['sha256'])
        provenance = cvm_package.package_provenance(source, directory / 'manifest.json', assets, runtime)
        if (
            packaged['source_sha256'] != runtime['sourceSHA256']
            or packaged['min_glibc'] != '2.35'
            or packaged['architecture'] != runtime['architecture']
            or packaged['provenance'] != provenance
        ):
            raise ValueError(f'Cube {stack} bundle source, ABI or provenance does not match its inputs')
        agent_sha = packaged['files']['runtime/cube-agent/cube-agent.ext4']['sha256']
        if agent_sha != guest_assets['files']['agent.ext4']:
            raise ValueError(f'Cube {stack} Guest Agent differs from the fixed asset manifest')
        result['stacks'][stack] = {
            'bundle': str(bundle),
            'sha256': receipt['sha256'],
            'state_abi': packaged['state_abi'],
            'cubelet_sha256': packaged['files'][cvm_stack.CUBELET]['sha256'],
            'shim_sha256': packaged['files'][cvm_stack.SHIM]['sha256'],
            'shim_version_sha256': packaged['files']['runtime/cube-shim/version']['sha256'],
            'guest_agent_sha256': agent_sha,
            'provenance': provenance,
            'artifacts': runtime['artifacts'],
        }
    first, second = result['stacks'].values()
    if first['state_abi'] != second['state_abi']:
        raise ValueError('Stack A and B persistent-state ABIs differ')
    for key in ('sha256', 'cubelet_sha256', 'shim_sha256', 'shim_version_sha256'):
        if first[key] == second[key]:
            raise ValueError(f'Stack A and B must have distinct {key}')
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--workspace', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--assets', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(verify(args.workspace.resolve(), args.output.resolve(), args.assets.resolve()), sort_keys=True))


if __name__ == '__main__':
    main()
