import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


PATH = Path(__file__).resolve().parents[1] / 'release_orchestrator.py'
SPEC = importlib.util.spec_from_file_location('release_orchestrator', PATH)
ORCHESTRATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ORCHESTRATOR)


def configuration(root):
    return {
        'version': 1,
        'paths': {'workspace': 'work', 'evidence': 'evidence', 'cache': 'cache'},
        'source_repository': str(root),
        'node_version': '22.23.0',
        'tool_sha256': {'node': '0' * 64, 'go': '0' * 64},
        'base_image': 'ubuntu:22.04',
        'image_prefix': 'brocade-ci-test',
        'install_missing_tools': False,
        'inputs': {},
    }


class ReleaseOrchestratorTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.config_file = self.root / 'environment.json'
        self.config_file.write_text(json.dumps(configuration(self.root)))
        self.config = ORCHESTRATOR.read_config(self.config_file)
        self.environment = ORCHESTRATOR.ReleaseEnvironment(self.config)
        for name in ('base', 'cvm', 'cube-a', 'cube-b'):
            source = self.environment.source(name)
            (source / 'scripts').mkdir(parents=True)
            (source / 'scripts/build-release.py').touch()

    def tearDown(self):
        self.temporary.cleanup()

    def test_default_jobs_overcommit_four_independent_builders(self):
        self.assertEqual(self.config['jobs'], {'base': 16, 'cvm': 16, 'cube-a': 12, 'cube-b': 12})
        self.assertEqual(sum(self.config['jobs'].values()), 56)

    def test_docker_environment_is_set_inside_container_not_only_on_host(self):
        with patch.object(self.environment, 'mount_git', return_value=[]):
            command = self.environment.container('probe', 'cube', self.root, extra_env={'TARGET_KIND': 'cube'})
        self.assertIn('TARGET_KIND=cube', command)
        self.assertEqual(command[command.index('TARGET_KIND=cube') - 1], '--env')
        self.assertNotIn('TARGET_KIND', os.environ)
        path = next(value for value in command if value.startswith('PATH='))
        self.assertIn('/home/build/tool-zig:', path)

    def test_container_mounts_all_candidate_sources_for_independent_verification(self):
        with patch.object(self.environment, 'mount_git', side_effect=lambda source: [str(source / '.git-parent')]):
            command = self.environment.container('verify', 'packager', self.environment.source('cube-a'), evidence=True)
        for name in ('base', 'cvm', 'cube-a', 'cube-b'):
            source = str(self.environment.source(name))
            self.assertIn(f'{source}:{source}', command)
            self.assertIn(str(self.environment.source(name) / '.git-parent'), command)
        evidence = str(self.environment.evidence)
        self.assertIn(f'{evidence}:{evidence}', command)

    def test_unowned_target_cache_is_rejected_before_compilation(self):
        target = self.environment.source('base') / 'target/brocade-agent-build'
        target.mkdir(parents=True)
        with patch.object(
            ORCHESTRATOR, 'checked', return_value=subprocess.CompletedProcess([], 0, b'sha256:new\n', b'')
        ):
            with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, 'unowned compiled caches'):
                self.environment.check_target('base')

    def test_target_from_another_image_is_rejected_without_deletion(self):
        source = self.environment.source('base')
        target = source / 'target/brocade-agent-build'
        target.mkdir(parents=True)
        (target / 'foreign-object').write_text('preserve')
        ORCHESTRATOR.atomic_json(
            source / 'target/.brocade-build-environment.json', {'version': 1, 'image': 'sha256:old'}
        )
        with patch.object(
            ORCHESTRATOR, 'checked', return_value=subprocess.CompletedProcess([], 0, b'sha256:new\n', b'')
        ):
            with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, 'another build environment'):
                self.environment.check_target('base')
        self.assertEqual((target / 'foreign-object').read_text(), 'preserve')

    def test_explicit_retirement_only_removes_owned_target(self):
        source = self.environment.source('base')
        target = source / 'target/brocade-agent-build'
        target.mkdir(parents=True)
        (target / 'old-object').write_text('old')
        ORCHESTRATOR.atomic_json(
            source / 'target/.brocade-build-environment.json', {'version': 1, 'image': 'sha256:old'}
        )
        self.environment.clear_owned_targets()
        self.assertFalse(target.exists())
        self.assertTrue(list((self.environment.evidence / 'retired-targets').glob('base-*.json')))

    def test_attempt_records_are_never_overwritten(self):
        runner = ORCHESTRATOR.Runner(self.environment.evidence / 'attempt', 'attempt')
        runner.execute([sys.executable, '-c', 'print("first")'])
        first = (runner.directory / 'execution.log').read_text()
        with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, 'preserve the existing attempt'):
            runner.execute([sys.executable, '-c', 'print("second")'])
        self.assertEqual((runner.directory / 'execution.log').read_text(), first)

    def test_nonzero_attempt_keeps_log_and_exit_record(self):
        runner = ORCHESTRATOR.Runner(self.environment.evidence / 'failed', 'failed')
        with self.assertRaises(ORCHESTRATOR.OrchestrationError):
            runner.execute([sys.executable, '-c', 'print("actual failure");raise SystemExit(7)'])
        self.assertEqual(json.loads(runner.record_path.read_text())['exit_code'], 7)
        self.assertIn('actual failure', runner.log.read_text())

    def test_kvm_rejects_missing_image_pin_before_creating_vms(self):
        image = self.root / 'os.img'
        image.write_bytes(b'not the expected image')
        for name in ('geoip', 'geosite'):
            (self.root / (name + '.dat')).write_bytes(b'asset')
        self.environment.config['inputs'].update(
            {
                'os_image': image,
                'cube_assets': self.root,
                'geoip': self.root / 'geoip.dat',
                'geosite': self.root / 'geosite.dat',
            }
        )
        self.environment.config['os_image_sha256'] = '0' * 64
        with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, 'OS image differs'):
            self.environment.kvm_inputs()

    def test_stack_b_generation_uses_a_real_source_patch(self):
        for name in ('cube-a', 'cube-b'):
            source = self.environment.source(name)
            for relative in (
                'components/cube/CubeShim/shim/build.rs',
                'components/cube/Cubelet/pkg/version/version.go',
            ):
                path = source / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('version = "0.0.0-dev"\n')
        with patch.object(
            ORCHESTRATOR,
            'checked',
            side_effect=lambda command, **kwargs: subprocess.CompletedProcess(
                command,
                0,
                b'head\n'
                if 'rev-parse' in command
                else b'diff --git a/version b/version\n-version = "0.0.0-dev"\n+version = "0.0.0-b"\n'
                if 'diff' in command
                else b'',
                b'',
            ),
        ):
            self.environment.prepare_sources()
        self.assertTrue(self.environment.config['stack_b_patch'].is_file())
        self.assertIn('-version = "0.0.0-dev"', self.environment.config['stack_b_patch'].read_text())
        self.assertIn('+version = "0.0.0-b"', self.environment.config['stack_b_patch'].read_text())

    def test_main_lab_stops_only_after_all_phases(self):
        root = self.environment.evidence / 'main'
        phases = []
        with (
            patch.object(self.environment, 'prepare_lab'),
            patch.object(
                self.environment, 'phase', side_effect=lambda _root, name, _command, **kwargs: phases.append(name)
            ),
        ):
            self.environment.main_acceptance(root, self.root / 'cvm', self.root / 'frozen')
        self.assertEqual(phases, [*ORCHESTRATOR.MAIN_PHASES, 'stop'])

    def test_stack_scenarios_resume_individually(self):
        root = self.environment.evidence / 'stack'
        evidence = root / 'stack-acceptance'
        evidence.mkdir(parents=True)
        state = {'cases': {name: {'verified': True} for name in ORCHESTRATOR.STACK_SCENARIOS}}
        (evidence / 'state.json').write_text(json.dumps(state))
        phases = []
        with patch.object(
            self.environment, 'phase', side_effect=lambda _root, name, _command, **kwargs: phases.append(name)
        ):
            self.environment.stack_acceptance(root, self.root / 'cvm')
        self.assertEqual(phases, ['stop'])

    def test_stack_rejects_rerunning_an_interrupted_mutation(self):
        root = self.environment.evidence / 'stack'
        evidence = root / 'stack-acceptance'
        evidence.mkdir(parents=True)
        (evidence / 'state.json').write_text(json.dumps({'cases': {'success': {'release_id': 'interrupted'}}}))
        with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, 'interrupted Stack scenarios'):
            self.environment.stack_acceptance(root, self.root / 'cvm')

    def test_failed_mutating_phase_is_not_repeated(self):
        root = self.environment.evidence / 'main'
        step = root / 'steps/bootstrap'
        ORCHESTRATOR.atomic_json(step / 'result.json', {'exit_code': 1})
        with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, 'new disposable run'):
            self.environment.phase(root, 'bootstrap', ['must-not-run'])
        self.assertTrue((step / 'result.json').is_file())

    def test_first_install_rejects_incomplete_evidence(self):
        root = self.environment.evidence / 'first-install'
        evidence = root / 'stack-acceptance'
        evidence.mkdir(parents=True)
        (evidence / 'first-install.json').write_text('{"download_fault": {"mode": "hash"}}')
        with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, 'new disposable lab'):
            self.environment.first_install(root, self.root / 'cvm')

    def test_failed_preflight_replaces_stale_success_report(self):
        with (
            patch.object(self.environment, 'host_preflight', return_value={'platform': 'linux'}),
            patch.object(self.environment, 'require_tools', return_value='head'),
            patch.object(self.environment, 'probe', side_effect=ORCHESTRATOR.OrchestrationError('compiler missing')),
        ):
            with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, '4 suites'):
                self.environment.preflight()
        report = json.loads((self.environment.evidence / 'preflight/report.json').read_text())
        self.assertEqual(report['status'], 'failed')
        self.assertIn('compiler missing', report['error'])

    def test_retryable_phase_archives_failed_attempt(self):
        root = self.environment.evidence / 'preflight'
        step = root / 'steps/base'
        ORCHESTRATOR.atomic_json(step / 'result.json', {'exit_code': 127})
        with patch.object(ORCHESTRATOR.Runner, 'execute') as execute:
            self.environment.phase(root, 'base', ['probe'], retryable=True)
        self.assertTrue((root / 'steps/base-attempt-2/result.json').is_file())
        execute.assert_called_once()

    def test_docker_builder_falls_back_when_buildx_is_absent(self):
        source = self.environment.source('base')
        commands = []

        def execute(command, **kwargs):
            commands.append((command, kwargs))

        with (
            patch.object(self.environment, 'host_preflight'),
            patch.object(self.environment, 'prepare_sources'),
            patch.object(self.environment, 'prepare_tools'),
            patch.object(self.environment, 'prepare_formatters'),
            patch.object(self.environment, 'pinned_base_image', return_value='ubuntu@sha256:' + '0' * 64),
            patch.object(ORCHESTRATOR, 'run', return_value=subprocess.CompletedProcess([], 1)),
            patch.object(
                ORCHESTRATOR, 'checked', return_value=subprocess.CompletedProcess([], 0, b'sha256:image\n', b'')
            ),
            patch.object(ORCHESTRATOR.Runner, 'execute', side_effect=execute),
        ):
            self.environment.prepare()
        self.assertTrue(commands)
        self.assertTrue(all(kwargs['env']['DOCKER_BUILDKIT'] == '0' for _command, kwargs in commands))

    def test_parallel_failures_are_aggregated(self):
        from concurrent.futures import Future

        futures = []
        for message in ('first failed', 'second failed'):
            future = Future()
            future.set_exception(ORCHESTRATOR.OrchestrationError(message))
            futures.append(future)
        with self.assertRaisesRegex(ORCHESTRATOR.OrchestrationError, '2 suites') as caught:
            ORCHESTRATOR.wait_all(futures, 'acceptance')
        self.assertIn('first failed', str(caught.exception))
        self.assertIn('second failed', str(caught.exception))


if __name__ == '__main__':
    unittest.main()
