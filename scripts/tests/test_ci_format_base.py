"""Exercise the actual CI range selection against shallow Git checkouts."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


def workflow_script():
    lines = (ROOT / '.github/workflows/ci.yml').read_text().splitlines()
    start = lines.index('      - name: Check all changed languages and configuration')
    start = lines.index('        run: |', start) + 1
    body = []
    for line in lines[start:]:
        if line and not line.startswith('          '):
            break
        body.append(line[10:])
    return '\n'.join(body) + '\n'


class CiFormatBaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')
        self.origin = self.root / 'origin'
        self.origin.mkdir()
        self.git(self.origin, 'init', '-q', '--initial-branch=main')
        script = self.origin / 'scripts/format.py'
        script.parent.mkdir()
        # Capture the formatter boundary without installing tools or replacing the range logic.
        script.write_text(
            textwrap.dedent("""\
            import json, pathlib, subprocess, sys
            base = sys.argv[sys.argv.index('--base') + 1]
            resolved = subprocess.check_output(['git', 'rev-parse', base], text=True).strip()
            files = subprocess.check_output(['git', 'diff', '--name-only', base, 'HEAD'], text=True).splitlines()
            pathlib.Path('observed.json').write_text(json.dumps({'base': resolved, 'files': files}))
        """)
        )
        self.commits = [self.commit('initial')]
        for name in ['agent.rs', 'frontend.ts', 'Cargo.toml']:
            (self.origin / name).write_text('fixture\n')
            self.commits.append(self.commit(name))
        self.git(self.origin, 'tag', 'v0.3.7')
        self.checkout = self.root / 'checkout'

    def git(self, directory, *arguments):
        return subprocess.check_output(
            ['git', *arguments], cwd=directory, env=self.env, stderr=subprocess.STDOUT, text=True
        ).strip()

    def commit(self, message):
        self.git(self.origin, 'add', '.')
        self.git(
            self.origin,
            '-c',
            'user.name=CI Fixture',
            '-c',
            'user.email=ci@example.invalid',
            '-c',
            'commit.gpgsign=false',
            'commit',
            '-qm',
            message,
        )
        return self.git(self.origin, 'rev-parse', 'HEAD')

    def clone(self, ref):
        self.git(
            self.root,
            'clone',
            '-q',
            '--depth=2',
            '--single-branch',
            '--branch',
            ref,
            self.origin.as_uri(),
            str(self.checkout),
        )
        self.assertEqual(self.git(self.checkout, 'rev-parse', '--is-shallow-repository'), 'true')

    def run_workflow(self, before=''):
        result = subprocess.run(
            ['bash', '-e', '-c', workflow_script()],
            cwd=self.checkout,
            env=self.env | {'FORMAT_BEFORE': before, 'FORMAT_DEFAULT_BRANCH': 'main'},
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads((self.checkout / 'observed.json').read_text())

    def test_tag_at_default_head_preserves_its_parent(self):
        self.clone('v0.3.7')
        self.assertNotIn('refs/remotes/origin/main', self.git(self.checkout, 'show-ref'))
        observed = self.run_workflow('0' * 40)
        self.assertEqual(observed, {'base': self.commits[-2], 'files': ['Cargo.toml']})

    def test_manual_default_branch_uses_the_previous_commit(self):
        self.clone('main')
        observed = self.run_workflow()
        self.assertEqual(observed, {'base': self.commits[-2], 'files': ['Cargo.toml']})

    def test_multi_commit_push_fetches_the_complete_before_range(self):
        self.clone('main')
        observed = self.run_workflow(self.commits[0])
        self.assertEqual(observed['base'], self.commits[0])
        self.assertEqual(observed['files'], ['Cargo.toml', 'agent.rs', 'frontend.ts'])

    def test_default_branch_ahead_of_tag_is_a_valid_base(self):
        (self.origin / 'ci.txt').write_text('forward CI fix\n')
        current = self.commit('advance main')
        self.clone('v0.3.7')
        self.assertEqual(self.run_workflow(), {'base': current, 'files': ['ci.txt']})


if __name__ == '__main__':
    unittest.main()
