#!/usr/bin/env python3
"""Independently check the retained Docker/KVM acceptance evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import re


def require(value, message):
    if not value:
        raise RuntimeError(message)


def load(path):
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    output = args.output
    manifest = load(output / 'manifest.json')
    for name, expected in manifest['inputs'].items():
        with (output / 'inputs' / name).open('rb') as stream:
            require(hashlib.file_digest(stream, 'sha256').hexdigest() == expected, f'changed input: {name}')
    summaries = {}
    expected_cases = {
        'environment', 'runtime-stall-isolated-and-recovers',
        'runtime-drip-deadline-and-resource-recovery', 'usage-drop-ack',
        'usage-truncate-ack', 'usage-503-ack', 'usage-usage-timeout-ack',
        'agent-crash-preserves-backlog-and-live-xray', 'ready-for-real-reboot',
        'offline-reboot-backlog-delivered',
    }
    for runtime in ['docker', 'kvm']:
        root = output / runtime
        if not root.exists():
            continue
        require(not (root / 'failure.txt').exists(), f'{runtime}: guest failure')
        result = load(root / 'result.json')
        require(result['passed'] is True, f'{runtime}: unsuccessful guest result')
        evidence = load(root / 'evidence.json')
        cases = {row['case']: row for row in evidence}
        require(set(cases) == expected_cases and len(evidence) == len(expected_cases) == result['cases'],
                f'{runtime}: missing or duplicate daemon cases')
        environment = cases['environment']
        require(environment['agent_sha256'] == manifest['inputs']['agent'], f'{runtime}: Agent identity')
        require(environment['xray_sha256'] == manifest['inputs']['xray'], f'{runtime}: Xray identity')
        tests = load(root / 'test-results.json')
        groups = tests['groups']
        require(len({row['filter'] for row in groups}) == len(groups), f'{runtime}: duplicate test group')
        native = set(tests['ignored_inventory']) - {tests['excluded_guest_docker_daemon_test']}
        require(native and tests['excluded_guest_docker_daemon_test'] in tests['ignored_inventory'],
                f'{runtime}: invalid native inventory')
        require(native == {row['filter'] for row in groups[3:]}, f'{runtime}: missing native test')
        require([row['filter'] for row in groups[:3]] == ['http::', 'desired_delivery_tests::', 'spool::tests::'],
                f'{runtime}: missing transport test group')
        for group in groups:
            text = (root / group['log']).read_text()
            match = re.search(r'test result: ok\. (\d+) passed; 0 failed; 0 ignored;', text)
            require(match and int(match[1]) == group['passed'] > 0, f'{runtime}: invalid test log {group}')
        stall = cases['runtime-stall-isolated-and-recovers']
        drip = cases['runtime-drip-deadline-and-resource-recovery']
        require(28 <= stall['deadline_secs'] <= 35 and 28 <= drip['deadline_secs'] <= 35,
                f'{runtime}: request deadline exceeded')
        require(stall['pids_before'] == stall['pids_after'] == drip['pids'], f'{runtime}: timeout restarted service')
        for case in [stall, drip]:
            require(any(row['label'] == 'alice@platform.acme#ing-a1b2' and any(
                source['ip'] == '198.18.0.2' and 'vless' in (source.get('protocols') or [])
                for source in row['sources']) for row in case['online_sources']),
                f'{runtime}: recovered runtime omitted the active source')
        require(stall['concurrent_usage'] > 0 and stall['concurrent_desired'] > 0,
                f'{runtime}: control traffic serialized behind runtime')
        require(stall['traffic_after']['tcp_samples'] > stall['traffic_before']['tcp_samples'] + 100,
                f'{runtime}: TCP did not progress during stall')
        require(stall['traffic_after']['udp_samples'] > stall['traffic_before']['udp_samples'] + 50,
                f'{runtime}: UDP did not progress during stall')
        require(drip['recovered']['fds'] <= drip['baseline']['fds'] + 12 and
                drip['recovered']['threads'] <= drip['baseline']['threads'] + 6,
                f'{runtime}: resource accumulation')
        ledger = load(root / 'ledger.json')
        for mode in ['drop', 'truncate', '503', 'usage-timeout']:
            case = cases['usage-' + mode + '-ack']
            require(case['attempts'] >= 2 and case['unique_commits'] == 1 and
                    case['retained_before_ack'] and case['removed_after_complete_ack'],
                    f'{runtime}: incorrect ACK handling: {mode}')
            require(case['pids'] == stall['pids_before'], f'{runtime}: ACK fault restarted service')
            require(ledger[case['key']]['sha256'] == case['payload_sha256'], f'{runtime}: replay payload changed')
            if mode == 'usage-timeout':
                require(28 <= case['fault_duration_secs'] <= 35, f'{runtime}: usage timeout exceeded')
        crash = cases['agent-crash-preserves-backlog-and-live-xray']
        require(crash['pids_before']['agent'] != crash['pids_after']['agent'] and
                crash['pids_before']['xray'] == crash['pids_after']['xray'], f'{runtime}: incorrect crash recovery')
        reboot = cases['offline-reboot-backlog-delivered']
        require(set(reboot['retained_keys']) == set(cases['ready-for-real-reboot']['owed_keys']),
                f'{runtime}: lost reboot checkpoint')
        for key in crash['retained_keys'] + reboot['retained_keys']:
            require(key in ledger, f'{runtime}: missing receipt {key}')
        for case in evidence:
            for key in ['traffic', 'traffic_before', 'traffic_after']:
                if key in case:
                    minimum = 0 if key == 'traffic_before' else 1
                    require(case[key]['tcp_samples'] >= minimum and case[key]['udp_samples'] >= minimum and
                            case[key]['errors'] == [], f'{runtime}: business flow errors')
        if runtime == 'docker':
            restart = load(root / 'container-restart.json')
            require(restart['before'] != restart['after'], 'container did not restart')
        else:
            acceleration = load(root / 'kvm-acceleration.json')
            require(acceleration == {'enabled': True, 'present': True}, 'hardware KVM not enabled')
            require(environment['boot_id'] != manifest['host_boot_id'], 'guest shares the host kernel boot')
            require(reboot['old_boot_id'] != reboot['new_boot_id'], 'VM did not reboot')
        summaries[runtime] = {'tests_passed': sum(row['passed'] for row in groups),
                              'native_tests_passed': len(native), 'daemon_cases_passed': len(cases),
                              'fault_recovery_cases_passed': len(cases) - 2,
                              'stall_deadline_secs': stall['deadline_secs'],
                              'drip_deadline_secs': drip['deadline_secs'],
                              'business_before_restart': crash['traffic'], 'receipts': len(ledger)}
    require(summaries, 'no runtime evidence')
    report = {'passed': True, 'runtimes': summaries}
    (output / 'verified.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
