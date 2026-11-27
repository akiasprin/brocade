import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location('cube_build', SCRIPTS / 'build-cube.py')
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class CubeBuildTests(unittest.TestCase):
    def test_bpf_headers_are_scoped_to_the_bpf_compiler(self):
        with tempfile.TemporaryDirectory(dir=SCRIPTS.parent / '.tools') as directory:
            root = Path(directory)
            libraries = root / 'sysroot'
            include = root / 'include'
            clang = root / 'clang'
            objcopy = root / 'llvm-objcopy'
            go = root / 'go'
            inputs = [
                libraries / 'libcap-ng.so',
                libraries / 'libseccomp.so',
                include / 'bpf/bpf_core_read.h',
                include / 'bpf/bpf_endian.h',
                include / 'bpf/bpf_helper_defs.h',
                include / 'bpf/bpf_helpers.h',
                include / 'bpf/bpf_tracing.h',
                clang,
                objcopy,
                go,
                root / 'components/cube/cubecow/include/cubecow.h',
            ]
            for path in inputs:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            (include / 'bpf/bpf_helper_defs.h').write_text('static long (* const bpf_loop)(void);\n')
            (root / 'components/cube/.go-version').write_text('1.27.4\n')
            (root / 'components/cube/Cubelet/third_party').mkdir(parents=True)
            output = root / 'target/cube/artifacts/x86_64'
            generated = {
                'cow': root / 'target/cube/cow/x86_64-unknown-linux-musl/release/libcubecow.a',
                'cubelet': output / 'cubelet',
                'containerd-shim-cube-rs': root
                / 'target/cube/shim/x86_64-unknown-linux-gnu/release/containerd-shim-cube-rs',
                'cube-runtime': root / 'target/cube/shim/x86_64-unknown-linux-gnu/release/cube-runtime',
                'inspector': output / 'lan-inspect',
            }
            environments = {}
            commands = {}

            def run(command, cwd, env, record, name, logs):
                environments[name] = env.copy()
                commands[name] = command
                if name in generated:
                    path = generated[name]
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b'artifact')
                if name == 'cow-cgo-archive':
                    Path(command[-1]).write_bytes(b'native archive')

            argv = [
                'build-cube.py',
                '--arch',
                'x86_64',
                '--sysroot-libs',
                str(libraries),
                '--bpf-include',
                str(include),
                '--clang',
                str(clang),
                '--go',
                str(go),
            ]
            env = {k: v for k, v in os.environ.items() if k != 'CPATH'}
            with (
                patch.object(BUILD, '__file__', str(root / 'scripts/build-cube.py')),
                patch.object(sys, 'argv', argv),
                patch.dict(os.environ, env, clear=True),
                patch.object(BUILD, 'source_digest', return_value='source'),
                patch.object(BUILD, 'validate_elf', return_value='2.34'),
                patch.object(BUILD.subprocess, 'check_output', return_value='go version go1.27.4 linux/amd64'),
                patch.object(BUILD, 'run', side_effect=run),
            ):
                BUILD.main()
            self.assertEqual(environments['bpf']['CPATH'], str(include))
            self.assertEqual(set(environments), {'bpf', 'cow-cgo-archive', *generated})
            self.assertEqual(
                commands['cow-cgo-archive'][:3],
                [str(objcopy), '--remove-section=.llvmbc', '--remove-section=.llvmcmd'],
            )
            for stage in {*generated, 'cow-cgo-archive'}:
                self.assertNotIn('CPATH', environments[stage], stage)

    def test_bpf_headers_require_loop_helper_and_record_digests(self):
        with tempfile.TemporaryDirectory(dir=SCRIPTS.parent / '.tools') as directory:
            include = Path(directory) / 'include'
            bpf = include / 'bpf'
            bpf.mkdir(parents=True)
            for name in ['bpf_core_read.h', 'bpf_endian.h', 'bpf_helper_defs.h', 'bpf_helpers.h', 'bpf_tracing.h']:
                (bpf / name).touch()
            with self.assertRaisesRegex(ValueError, 'does not declare required bpf_loop'):
                BUILD.bpf_header_inputs(include)
            (bpf / 'bpf_helper_defs.h').write_text('static long (* const bpf_loop)(void);\n')
            inputs = BUILD.bpf_header_inputs(include)
            self.assertEqual(
                set(inputs),
                {
                    'bpf/bpf_core_read.h',
                    'bpf/bpf_endian.h',
                    'bpf/bpf_helper_defs.h',
                    'bpf/bpf_helpers.h',
                    'bpf/bpf_tracing.h',
                },
            )
            self.assertTrue(all(len(value) == 64 for value in inputs.values()))


if __name__ == '__main__':
    unittest.main()
