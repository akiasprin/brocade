import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import cvm_package


SPEC = importlib.util.spec_from_file_location('package_cube_candidate', SCRIPTS / 'package_cube_candidate.py')
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)


class PackageCubeCandidateTests(unittest.TestCase):
    def test_warm_package_follows_runtime_and_guest_asset_manifests(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scripts = root / 'scripts'
            scripts.mkdir()
            for name in ('build-release.py', 'cvm_package.py', 'cvm_stack.py', 'cvm_host.py'):
                (scripts / name).symlink_to(SCRIPTS / name)
            assets = root / 'assets'
            assets.mkdir()
            asset_manifest = assets / 'manifest.json'
            asset_manifest.write_text('{}')
            directory = root / 'target/cube/artifacts/x86_64'
            directory.mkdir(parents=True)
            manifest_path = directory / 'manifest.json'
            manifest = {'result': 'build_complete', 'architecture': 'x86_64', 'shimVersion': 'v1'}
            manifest_path.write_text(json.dumps(manifest))

            def assemble(_root, build_manifest, asset_dir, output):
                output.write_bytes(build_manifest.read_bytes() + (asset_dir / 'manifest.json').read_bytes())

            with patch.object(cvm_package, 'prepare', side_effect=assemble) as prepare:
                original = PACKAGER.package(root, assets, 'x86_64')
                self.assertEqual(PACKAGER.package(root, assets, 'x86_64'), original)
                self.assertEqual(prepare.call_count, 1)
                manifest['shimVersion'] = 'v2'
                manifest_path.write_text(json.dumps(manifest))
                rebuilt = PACKAGER.package(root, assets, 'x86_64')
                self.assertNotEqual(rebuilt['sha256'], original['sha256'])
                asset_manifest.write_text('{"agent.ext4":"updated"}')
                repackaged = PACKAGER.package(root, assets, 'x86_64')
                self.assertNotEqual(repackaged['sha256'], rebuilt['sha256'])
                self.assertTrue(Path(original['bundle']).is_file())
                self.assertTrue(Path(rebuilt['bundle']).is_file())
                self.assertEqual(prepare.call_count, 3)
