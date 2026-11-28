#!/usr/bin/env python3
"""Assemble a Cube node bundle from an existing verified runtime manifest."""

import argparse
import importlib.util
import json
from pathlib import Path
import os
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
    manifest_path = directory / 'manifest.json'
    manifest = json.loads(manifest_path.read_text())
    if manifest.get('result') != 'build_complete' or manifest.get('architecture') != architecture:
        raise ValueError('Cube runtime is incomplete or has the wrong architecture')
    assets = assets.resolve()
    provenance = cvm_package.package_provenance(root, manifest_path, assets, manifest)
    receipt_path = directory / 'bundle.json'
    if receipt_path.exists():
        receipt = json.loads(receipt_path.read_text())
        if receipt.get('provenance') == provenance and receipt.get('architecture') == architecture:
            bundle = Path(receipt['bundle'])
            if receipt['sha256'] != build_release.digest(bundle, timer):
                raise ValueError('existing Cube bundle differs from its receipt')
            return receipt
    with tempfile.TemporaryDirectory(prefix='bundle-', dir=directory) as temporary:
        bundle = Path(temporary) / 'bundle.tar.gz'
        cvm_package.prepare(root, manifest_path, assets, bundle)
        if provenance != cvm_package.package_provenance(root, manifest_path, assets, manifest):
            raise ValueError('Cube packaging inputs changed during assembly')
        sha = build_release.digest(bundle, timer)
        final = directory / f'brocade-cvm-{architecture}-{sha}.tar.gz'
        try:
            os.link(bundle, final)
        except FileExistsError:
            if final.is_symlink() or build_release.digest(final, timer) != sha:
                raise ValueError(f'existing Cube bundle differs from its content identity: {final}') from None
        receipt = {
            'bundle': str(final),
            'sha256': sha,
            'architecture': architecture,
            'provenance': provenance,
            'started_at': timer.started_at,
            'completed_at': build_release.timestamp(),
            'duration_seconds': round(time.monotonic() - timer.started, 3),
            'deployment_ready': False,
        }
        temporary_receipt = Path(temporary) / 'bundle.json'
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
