#!/usr/bin/env python3
"""Assemble a Cube node bundle from an existing verified runtime manifest."""

import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tempfile
import time


def package(root, assets, architecture):
    root = root.resolve()
    sys.path.insert(0, str(root / 'scripts'))
    import cvm_package

    spec = importlib.util.spec_from_file_location('build_release', root / 'scripts/build-release.py')
    build_release = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(build_release)
    timer = build_release.BuildTimer()
    directory = root / 'target/cube/artifacts' / architecture
    manifest = json.loads((directory / 'manifest.json').read_text())
    if manifest.get('result') != 'build_complete':
        raise ValueError('Cube runtime is not complete')
    receipt_path = directory / 'bundle.json'
    if receipt_path.exists():
        receipt = json.loads(receipt_path.read_text())
        bundle = Path(receipt['bundle'])
        if receipt['sha256'] != build_release.digest(bundle, timer):
            raise ValueError('existing Cube bundle differs from its receipt')
        return receipt
    with tempfile.TemporaryDirectory(prefix='bundle-', dir=directory) as temporary:
        bundle = Path(temporary) / 'bundle.tar.gz'
        cvm_package.prepare(root, directory / 'manifest.json', assets.resolve(), bundle)
        sha = build_release.digest(bundle, timer)
        final = directory / f'brocade-cvm-{architecture}-{sha}.tar.gz'
        if final.exists():
            raise ValueError(f'preserve an existing candidate bundle: {final}')
        shutil.copyfile(bundle, final)
        if build_release.digest(final, timer) != sha:
            raise ValueError('CVM install bundle differs from its content identity')
        final.chmod(0o600)
        receipt = {
            'bundle': str(final),
            'sha256': sha,
            'architecture': architecture,
            'started_at': timer.started_at,
            'completed_at': build_release.timestamp(),
            'duration_seconds': round(time.monotonic() - timer.started, 3),
            'deployment_ready': False,
        }
        temporary_receipt = receipt_path.with_suffix('.tmp')
        temporary_receipt.write_text(json.dumps(receipt, indent=2) + '\n')
        temporary_receipt.replace(receipt_path)
        return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--assets', type=Path, required=True)
    parser.add_argument('--architecture', default='x86_64', choices=['x86_64', 'aarch64'])
    args = parser.parse_args()
    print(json.dumps(package(args.root, args.assets, args.architecture), sort_keys=True))


if __name__ == '__main__':
    main()
