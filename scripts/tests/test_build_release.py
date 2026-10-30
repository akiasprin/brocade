import argparse
import contextlib
from datetime import datetime, timedelta
import fcntl
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


PATH = Path(__file__).resolve().parents[1] / 'build-release.py'
SPEC = importlib.util.spec_from_file_location('release_build', PATH)
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class ReleaseBuildTests(unittest.TestCase):
    def assert_utc_timestamp(self, value):
        parsed = datetime.fromisoformat(value)
        self.assertIsNotNone(parsed.tzinfo)
        self.assertEqual(parsed.utcoffset(), timedelta(0))

    def test_temporary_and_archived_source_roots_are_rejected(self):
        for root in ['/tmp/source', '/var/tmp/source', '/work/project/target/releases/r1/source']:
            with self.assertRaises(BUILD.BuildError):
                BUILD.stable_root(Path(root))
        BUILD.stable_root(Path('/work/project'))

    def test_elapsed_time_beyond_the_former_budget_does_not_stop_a_build(self):
        with tempfile.TemporaryDirectory() as directory:
            timer = BUILD.BuildTimer(started=time.monotonic() - 1000)
            result = BUILD.run([sys.executable, '-c', 'import time; time.sleep(0.15); print("complete")'],
                               Path(directory), os.environ.copy(), timer)
            self.assertEqual(result, 'complete')

    def test_quiet_process_reports_elapsed_time_even_after_closing_its_output(self):
        for close_output in ['', 'os.close(1); os.close(2);']:
            with self.subTest(close_output=close_output), tempfile.TemporaryDirectory() as directory, \
                    patch.object(BUILD, 'PROGRESS_INTERVAL_SECONDS', 0.02), \
                    contextlib.redirect_stderr(io.StringIO()) as messages:
                timer = BUILD.BuildTimer()
                timer.stage = 'dual_architecture_build'
                reports = []
                timer.on_progress = lambda: reports.append(time.monotonic() - timer.started)
                BUILD.run([sys.executable, '-c',
                           f'import os, time; {close_output} time.sleep(0.15)'],
                          Path(directory), os.environ.copy(), timer)
                self.assertTrue(reports)
                self.assertIn('dual_architecture_build running, started ', messages.getvalue())
                self.assertIn('stage ', messages.getvalue())
                self.assertIn('total ', messages.getvalue())
                self.assertIn(timer.stage_started_at, messages.getvalue())

    def test_manual_cancellation_cleans_up_the_child_process_group(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / 'pids.json'
            timer = BUILD.BuildTimer()
            started = time.monotonic()
            def cancel_when_ready():
                if marker.exists():
                    raise KeyboardInterrupt()
                if time.monotonic() - started > 5:
                    self.fail('child did not start the cancellation fixture')
            command = '''import json, os, subprocess, sys, time
from pathlib import Path
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
marker = Path(sys.argv[1] + '.tmp')
marker.write_text(json.dumps([os.getpid(), child.pid]))
marker.replace(sys.argv[1])
time.sleep(30)
'''
            pids = []
            def running(pid):
                try:
                    # A reaped zombie can briefly become X (dead) before /proc disappears.
                    value = Path(f'/proc/{pid}/stat').read_text()
                    state = value.rsplit(')', 1)[1].split()[0]
                    return state not in ('Z', 'X', 'x')
                except (FileNotFoundError, ProcessLookupError):
                    return False
            try:
                with patch.object(timer, 'tick', side_effect=cancel_when_ready):
                    with self.assertRaises(KeyboardInterrupt):
                        BUILD.run([sys.executable, '-c', command, str(marker)], root,
                                  os.environ.copy(), timer)
                pids = json.loads(marker.read_text())
                until = time.monotonic() + 1
                while any(running(pid) for pid in pids) and time.monotonic() < until:
                    time.sleep(0.01)
                self.assertFalse(any(running(pid) for pid in pids))
            finally:
                for pid in pids:
                    if running(pid):
                        try:
                            os.kill(pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def test_build_output_is_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(BUILD.BuildError, 'output exceeds'):
                BUILD.run([sys.executable, '-c', 'import sys; sys.stdout.write("x" * 20000000)'],
                          Path(directory), os.environ.copy(), BUILD.BuildTimer())

    def test_failed_preflight_replaces_previous_success_without_admitting_old_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'target/brocade-release'
            state.mkdir(parents=True)
            result = state / 'preparation.json'
            result.write_text('{"build_complete":true}')
            with patch.object(BUILD, 'stable_root'), patch.object(BUILD, 'run', return_value=str(root)), \
                    patch.object(BUILD, 'go_binary', side_effect=BUILD.BuildError('toolchain unavailable')), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(BUILD.BuildError, 'toolchain unavailable'):
                    BUILD.build(argparse.Namespace(jobs=4), root, BUILD.BuildTimer())
            record = json.loads(result.read_text())
            self.assertEqual(record['status'], 'failed')
            self.assertFalse(record['build_complete'])
            self.assertFalse(record['deployment_ready'])
            self.assertEqual(record['artifacts'], {})
            self.assertEqual(record['stages'][0]['name'], 'preflight')
            self.assertEqual(record['stages'][0]['status'], 'failed')
            self.assertGreaterEqual(record['stages'][0]['seconds'], 0)
            self.assertGreaterEqual(record['elapsed_seconds'], record['stages'][0]['seconds'])
            self.assertNotIn('active_stage', record)
            for event in [record, *record['stages']]:
                self.assert_utc_timestamp(event['started_at'])
                self.assert_utc_timestamp(event['ended_at'])

    def test_cancellation_keeps_finish_times_and_monotonic_duration_when_wall_clock_moves_back(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            wall_start = '2026-10-29T01:00:00.000+00:00'
            wall_end = '2026-10-29T00:59:00.000+00:00'
            with patch.object(BUILD, 'timestamp', return_value=wall_start) as wall, \
                    patch.object(BUILD, 'stable_root'), patch.object(BUILD, 'run', return_value=str(root)), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()) as messages:
                timer = BUILD.BuildTimer(started=time.monotonic() - 1000)
                def cancel(*args):
                    wall.return_value = wall_end
                    raise KeyboardInterrupt()
                with patch.object(BUILD, 'go_binary', side_effect=cancel):
                    with self.assertRaises(KeyboardInterrupt):
                        BUILD.build(argparse.Namespace(jobs=4), root, timer)
            record = json.loads((root / 'target/brocade-release/preparation.json').read_text())
            self.assertEqual(record['status'], 'cancelled')
            self.assertFalse(record['build_complete'])
            self.assertFalse(record['deployment_ready'])
            self.assertEqual(record['started_at'], wall_start)
            self.assertEqual(record['ended_at'], wall_end)
            self.assertGreaterEqual(record['elapsed_seconds'], 1000)
            stage = record['stages'][0]
            self.assertEqual(stage['status'], 'cancelled')
            self.assertEqual(stage['started_at'], wall_start)
            self.assertEqual(stage['ended_at'], wall_end)
            self.assertGreaterEqual(stage['seconds'], 0)
            self.assertIn(f'started {wall_start}, ended {wall_end}', messages.getvalue())
            self.assertIn('preflight cancelled', messages.getvalue())

    def test_contending_build_fails_immediately_without_overwriting_owner_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'target/brocade-release'
            state.mkdir(parents=True)
            result = state / 'preparation.json'
            result.write_text('{"owner":"first"}')
            with (state / 'lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with patch.object(BUILD, 'stable_root'), contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaisesRegex(BUILD.BuildError, 'another release build'):
                        BUILD.build(argparse.Namespace(jobs=4), root, BUILD.BuildTimer())
            self.assertEqual(json.loads(result.read_text()), {'owner': 'first'})

    def test_rejected_checkout_still_reports_the_attempt_start_and_finish(self):
        with patch.object(sys, 'argv', [str(PATH)]), \
                patch.object(BUILD.os, 'umask'), \
                patch.object(BUILD, 'stable_root', side_effect=BUILD.BuildError('checkout rejected')), \
                contextlib.redirect_stderr(io.StringIO()) as messages:
            self.assertEqual(BUILD.main(), 1)
        lines = messages.getvalue().splitlines()
        self.assertIn('release build started', lines[0])
        self.assertIn('checkout rejected', lines[1])
        self.assertIn('release build failed, started ', lines[2])
        self.assertIn(', ended ', lines[2])
        for line in [lines[0], lines[2]]:
            self.assert_utc_timestamp(line.split(']', 1)[0][1:])

    def test_long_build_persists_progress_and_accepts_only_this_runs_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'target/brocade-release'
            triples = ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu']
            sysroot = root / 'sysroot'
            for triple in triples + ['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl']:
                (sysroot / 'lib/rustlib' / triple / 'lib').mkdir(parents=True)
            reports = []
            def command(args, cwd, env, timer, log=None):
                if args[0] == 'git':
                    return str(root)
                if args[0] == 'rustc':
                    return str(sysroot)
                if args[0] == 'cargo':
                    timer.next_report = 0
                    timer.tick()
                    reports.append(json.loads((state / 'preparation.json').read_text()))
                    for triple in triples:
                        for name in ['brocade-console', 'brocade-launcher']:
                            path = state / 'cargo' / triple / 'release' / name
                            path.parent.mkdir(parents=True, exist_ok=True)
                            path.write_bytes(f'{triple}/{name}'.encode())
                    return ''
                return '0.16.0'
            with patch.dict(os.environ, {}, clear=True), patch.object(BUILD, 'stable_root'), \
                    patch.object(BUILD, 'run', command), patch.object(BUILD, 'go_binary', return_value='/tool/go'), \
                    patch.object(BUILD.shutil, 'which', side_effect=lambda tool: f'/tool/{tool}'), \
                    patch.object(BUILD.os, 'uname', return_value=argparse.Namespace(machine='x86_64')), \
                    patch.object(BUILD, 'inputs', return_value='unchanged-source'), \
                    patch.object(BUILD, 'frontend', return_value=root / 'dist'), \
                    patch.object(BUILD, 'agents', return_value=['cargo', 'build']), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()) as messages:
                BUILD.build(argparse.Namespace(jobs=4), root,
                            BUILD.BuildTimer(started=time.monotonic() - 1000))
            self.assertEqual(reports[0]['status'], 'running')
            self.assertEqual(reports[0]['active_stage']['name'], 'dual_architecture_build')
            self.assertGreaterEqual(reports[0]['elapsed_seconds'], 1000)
            self.assert_utc_timestamp(reports[0]['started_at'])
            self.assertIsNone(reports[0]['ended_at'])
            self.assert_utc_timestamp(reports[0]['active_stage']['started_at'])
            self.assertIsNone(reports[0]['active_stage']['ended_at'])
            record = json.loads((state / 'preparation.json').read_text())
            self.assertEqual(record['status'], 'built')
            self.assertTrue(record['build_complete'])
            self.assertFalse(record['deployment_ready'])
            self.assertEqual(len(record['artifacts']), 4)
            self.assertEqual([stage['name'] for stage in record['stages']],
                             ['preflight', 'frontend', 'dual_architecture_agent',
                              'dual_architecture_build', 'verification'])
            self.assertTrue(all(stage['status'] == 'complete' for stage in record['stages']))
            self.assertNotIn('active_stage', record)
            for event in [record, *record['stages']]:
                self.assert_utc_timestamp(event['started_at'])
                self.assert_utc_timestamp(event['ended_at'])
                self.assertIn(f'started {event["started_at"]}, ended {event["ended_at"]}',
                              messages.getvalue())

    def test_verified_frontend_cache_is_reused_but_tampering_forces_a_rebuild(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'frontend').mkdir()
            state = root / 'state'
            state.mkdir()
            calls = []
            def command(args, cwd, env, timer, log=None):
                calls.append(args)
                if args[:3] == ['npm', 'run', 'build']:
                    dist = state / 'frontend-dist'
                    dist.mkdir(exist_ok=True)
                    (dist / 'index.html').write_text('verified')
                return 'version'
            with patch.object(BUILD, 'inputs', return_value='source'), patch.object(BUILD, 'run', command):
                BUILD.frontend(root, state, {}, BUILD.BuildTimer())
                self.assertEqual(sum(c[:2] == ['npm', 'ci'] for c in calls), 1)
                BUILD.frontend(root, state, {}, BUILD.BuildTimer())
                self.assertEqual(sum(c[:2] == ['npm', 'ci'] for c in calls), 1)
                (state / 'frontend-dist/index.html').write_text('modified')
                BUILD.frontend(root, state, {}, BUILD.BuildTimer())
                self.assertEqual(sum(c[:2] == ['npm', 'ci'] for c in calls), 2)

    def test_agents_use_one_dual_target_cargo_graph_and_feed_console_overrides(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = root / 'target/brocade-release'
            state.mkdir(parents=True)
            commands = []
            def command(args, cwd, env, timer, log=None):
                commands.append((args, env.copy()))
                target = root / 'target/brocade-agent-build'
                for triple in ['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl']:
                    binary = target / triple / 'release/brocade-agent'
                    binary.parent.mkdir(parents=True, exist_ok=True)
                    binary.write_bytes(b'agent')
                return ''
            env = {}
            with patch.object(BUILD, 'run', command):
                BUILD.agents(root, state, env, BUILD.BuildTimer(), 4, '/tool/zig')
            self.assertEqual(len(commands), 1)
            args, child_env = commands[0]
            targets = [args[i + 1] for i, value in enumerate(args) if value == '--target']
            self.assertEqual(targets, ['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl'])
            self.assertIn('CC_x86_64_unknown_linux_musl', child_env)
            self.assertIn('CC_aarch64_unknown_linux_musl', child_env)
            self.assertTrue(env['BROCADE_AGENT_BIN_X86_64'].endswith('/x86_64-unknown-linux-musl/release/brocade-agent'))
            self.assertTrue(env['BROCADE_AGENT_BIN_AARCH64'].endswith('/aarch64-unknown-linux-musl/release/brocade-agent'))


if __name__ == '__main__':
    unittest.main()
