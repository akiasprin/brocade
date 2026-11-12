import ast
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('repo_format', ROOT / 'scripts/format.py')
FORMAT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FORMAT)


class FormatTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.root_patch = patch.object(FORMAT, 'ROOT', self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        self.git('init', '-q')
        for name in ['format-tools.json', 'pyproject.toml', 'rustfmt.toml', 'rust-toolchain.toml', '.prettierrc.cjs']:
            shutil.copyfile(ROOT / name, self.root / name)
        self.write('frontend/.prettierrc.json', (ROOT / 'frontend/.prettierrc.json').read_text())
        self.write('frontend/package-lock.json', (ROOT / 'frontend/package-lock.json').read_text())
        self.write('components/xray-core/.go-version', (ROOT / 'components/xray-core/.go-version').read_text())
        binary = self.root / 'frontend/node_modules/.bin/prettier'
        binary.parent.mkdir(parents=True)
        binary.symlink_to((ROOT / 'frontend/node_modules/.bin/prettier').resolve())
        self.write('.gitignore', '.tools/\nfrontend/node_modules/\n')
        self.git('add', '.')
        self.git(
            '-c',
            'user.name=Formatter Fixture',
            '-c',
            'user.email=formatter@example.invalid',
            'commit',
            '-qm',
            'fixture',
        )

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, stderr=subprocess.STDOUT)

    def write(self, name, body):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
        return Path(name)

    def test_real_formatters_are_idempotent_and_preserve_the_index(self):
        samples = {
            'example.rs': 'fn main(){println!("example");}\n',
            'example.go': 'package main\nfunc main(){println("example")}\n',
            'example.py': 'def example( value ):\n return {"answer":value}\n',
            'example.sh': '#!/bin/sh\nif true;then\necho example\nfi\n',
            'example.ts': 'const example={answer:"value"}\n',
            'example.json': '{"answer":1}\n',
            'example.yaml': 'answer: [1,2]\n',
            'example.md': '# Example\n\n-   answer\n',
            'example.svg': '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><path d="M0 0"/></svg>\n',
            'example.toml': 'answer = 1  \r\n',
        }
        files = [self.write(name, body) for name, body in samples.items()]
        original = {name: (self.root / name).read_bytes() for name in samples}
        self.git('add', '--', *samples)
        index = self.git('write-tree')
        with contextlib.redirect_stdout(io.StringIO()):
            FORMAT.format_files(files, False, False)
            FORMAT.format_files(files, True, False)
        after = {name: (self.root / name).read_bytes() for name in samples}
        self.assertTrue(all(original[name] != after[name] for name in samples))
        self.assertEqual(index, self.git('write-tree'), 'formatting must not stage user edits')
        self.assertEqual(ast.dump(ast.parse(original['example.py'])), ast.dump(ast.parse(after['example.py'])))
        self.assertEqual(json.loads(original['example.json']), json.loads(after['example.json']))
        with contextlib.redirect_stdout(io.StringIO()):
            FORMAT.format_files(files, False, False)
        self.assertEqual(after, {name: (self.root / name).read_bytes() for name in samples})

    def test_check_reports_a_real_format_error_without_writing(self):
        path = self.write('example.py', 'def example( value ):\n return value\n')
        before = (self.root / path).read_bytes()
        with self.assertRaises(subprocess.CalledProcessError), contextlib.redirect_stdout(io.StringIO()):
            FORMAT.format_files([path], True, False)
        self.assertEqual(before, (self.root / path).read_bytes())

    def test_changed_scope_includes_untracked_but_preserves_unmodified_vendor_files(self):
        self.write('tracked.py', 'value = 1\n')
        self.write('components/vendor/untouched.py', 'def untouched( ):pass\n')
        self.git('add', '.')
        self.git(
            '-c',
            'user.name=Formatter Fixture',
            '-c',
            'user.email=formatter@example.invalid',
            'commit',
            '-qm',
            'baseline',
        )
        self.write('tracked.py', 'value=2\n')
        self.write('new.py', 'value=3\n')
        self.write('.tools/private.py', 'value=4\n')
        files = FORMAT.select_files(FORMAT.git_paths(None))
        self.assertEqual(files, [Path('new.py'), Path('tracked.py')])
        vendor = (self.root / 'components/vendor/untouched.py').read_bytes()
        with contextlib.redirect_stdout(io.StringIO()):
            FORMAT.format_files(files, False, False)
        self.assertEqual(vendor, (self.root / 'components/vendor/untouched.py').read_bytes())
        self.assertEqual((self.root / '.tools/private.py').read_text(), 'value=4\n')

    def test_missing_or_ignored_paths_cannot_produce_a_false_pass(self):
        self.write('.tools/private.py', 'value=1\n')
        for path in ['missing.py', '.tools/private.py']:
            with self.subTest(path=path), self.assertRaises(ValueError):
                FORMAT.select_files([path])

    def test_symlinks_and_paths_outside_the_checkout_are_rejected(self):
        with tempfile.TemporaryDirectory() as outside:
            target = Path(outside) / 'outside.py'
            target.write_text('value=1\n')
            (self.root / 'link.py').symlink_to(target)
            for path in ['link.py', str(target)]:
                with self.subTest(path=path), self.assertRaises(ValueError):
                    FORMAT.select_files([path])
            self.assertEqual(target.read_text(), 'value=1\n')

    def test_immutable_sql_and_golden_bytes_are_preserved(self):
        migration = self.write('crates/example/migrations/0001_init.sql', 'select  1;  \n')
        golden = self.write('crates/example/tests/golden/result.json', '{"answer":1}')
        lock = self.write('package-lock.json', '{"generated":true}')
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(FORMAT.select_files([str(migration), str(golden), str(lock)]), [])
        self.assertEqual((self.root / migration).read_text(), 'select  1;  \n')
        self.assertEqual((self.root / golden).read_text(), '{"answer":1}')
        self.assertEqual((self.root / lock).read_text(), '{"generated":true}')

    def test_single_rust_file_does_not_format_its_sibling_module(self):
        parent = self.write('parent.rs', 'mod child;\nfn main(){}\n')
        child = self.write('child.rs', 'fn untouched( ){ }\n')
        before = (self.root / child).read_bytes()
        with contextlib.redirect_stdout(io.StringIO()):
            FORMAT.format_files([parent], False, False)
        self.assertEqual(before, (self.root / child).read_bytes())


if __name__ == '__main__':
    unittest.main()
