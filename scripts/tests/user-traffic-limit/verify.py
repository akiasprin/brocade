#!/usr/bin/env python3
"""Check completed real-process evidence, including local ingress limits and control-plane independence."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def verify(root):
    results = root / 'results'
    report = json.loads((results / 'complete.json').read_text())
    cases = {row['case']: row for row in report['summaries']}
    expected = ['unlimited_shared_1ms', 'limited_single_1ms', 'limited_shared_1ms',
                'limited_four_nodes_1ms', 'limited_shared_no_sleep', 'limited_shared_rtt80',
                'mixed_bulk_small', 'isolated_user_app', 'full_single_tcp', 'full_four_tcp',
                'full_four_udp', 'full_same_node_four_tcp', 'budget_latency_2300ms', 'policy_latency_2300ms', 'outage_recovery', 'hot_policy_existing_connections', 'unlimited_after']
    checks = {'all_expected_cases_present': set(expected) == set(cases)}
    inputs = json.loads((root / 'inputs.json').read_text())
    for name in ('agent', 'console', 'xray'):
        with (root / 'inputs' / name).open('rb') as stream:
            checks[name + '_pinned_binary_unchanged'] = hashlib.file_digest(stream, 'sha256').hexdigest() == inputs[name]['sha256']
    for name in expected:
        row = cases.get(name)
        if row is None:
            continue
        checks[name + '_nonempty'] = row['count'] >= 10
        if name != 'hot_policy_existing_connections':
            checks[name + '_no_errors'] = row['errors'] == 0
    for name in ['unlimited_shared_1ms', 'limited_single_1ms', 'limited_shared_1ms',
                 'limited_four_nodes_1ms', 'limited_shared_no_sleep', 'unlimited_after']:
        row = cases[name]
        checks[name + '_warm_tail_under100ms'] = row['warm']['max_ms'] is not None and row['warm']['max_ms'] < 100
    checks['rtt80_local_warm_tail_under200ms'] = cases['limited_shared_rtt80']['warm']['max_ms'] < 200
    checks['small_request_fairness_under_bulk_load'] = cases['mixed_bulk_small']['warm_per_stream']['5']['max_ms'] < 100
    checks['no_remote_budget_requests'] = all(row['budget_requests'] == 0 for row in cases.values())
    for worker in ('8', '9'):
        checks['independent_worker_' + worker] = cases['isolated_user_app']['warm_per_stream'][worker]['max_ms'] < 100
    for name in ('full_single_tcp', 'full_same_node_four_tcp', 'full_four_tcp', 'full_four_udp', 'budget_latency_2300ms', 'policy_latency_2300ms'):
        row = cases[name]
        ratio = row['actual_payload_bps'] / (row['rate_bps'] * len(row['nodes']))
        checks[name + '_independent_ingress_ceiling_and_utilization'] = .8 <= ratio <= 1.15
    events = [json.loads(line) for line in (results / 'events.jsonl').read_text().splitlines()]
    samples = []
    for line in (results / 'samples.jsonl').open():
        row = json.loads(line)
        if row['case'] in ('outage_recovery', 'hot_policy_existing_connections'):
            samples.append(row)
    down = next(e for e in events if e['kind'] == 'outage_start')
    up = next(e for e in events if e['kind'] == 'outage_end')
    completed_ns = lambda r: r['mono_ns'] + int(r['latency_ms'] * 1_000_000)
    checks['outage_keeps_local_policy_and_forwarding'] = any(
        r['ok'] and down['mono_ns']+2_500_000_000 < completed_ns(r) < up['mono_ns']
        for r in samples if r['case'] == 'outage_recovery')
    checks['outage_has_no_rejections'] = cases['outage_recovery']['errors'] == 0
    checks['outage_recovers'] = any(r['ok'] and r['mono_ns'] > up['mono_ns']+1_000_000_000
                                  for r in samples if r['case'] == 'outage_recovery')
    paused = next(e for e in events if e['kind'] == 'policy_change' and not e['available'])
    restored = next(e for e in events if e['kind'] == 'policy_change' and e['available'] and e['mono_ns'] > paused['mono_ns'])
    hot = [r for r in samples if r['case'] == 'hot_policy_existing_connections']
    checks['pause_stops_existing_connections'] = not any(
        r['ok'] and paused['mono_ns']+2_000_000_000 < completed_ns(r) < restored['mono_ns'] for r in hot)
    checks['pause_has_observed_rejections'] = cases['hot_policy_existing_connections']['errors'] > 0
    checks['policy_restore_recovers'] = any(r['ok'] and r['mono_ns'] > restored['mono_ns']+1_000_000_000 for r in hot)
    changes = [e for e in events if e['kind'] == 'policy_change']
    def phase_bps(begin, end):
        start = begin['mono_ns'] + 1_000_000_000
        finish = end['mono_ns']
        payload = sum(r['bytes'] * 16 for r in hot if r['ok'] and r['mono_ns'] >= start and completed_ns(r) < finish)
        return payload / ((finish - start) / 1_000_000_000)
    checks['hot_rate_change_applied_to_existing_connections'] = .6e6*len(cases['hot_policy_existing_connections']['nodes']) <= phase_bps(changes[0], changes[1]) <= 1.15e6*len(cases['hot_policy_existing_connections']['nodes'])
    checks['hot_limit_removal_restores_throughput'] = phase_bps(changes[1], changes[2]) > 10e6
    names = json.loads((root / 'names.json').read_text())
    checks['containers_removed'] = all(subprocess.run(['docker', 'inspect', names[k]], capture_output=True).returncode != 0
                                       for k in ('lab', 'db'))
    evidence = dict(passed=all(checks.values()), checks=checks, summaries=report['summaries'])
    (root / 'verification.json').write_text(json.dumps(evidence, indent=2))
    print(json.dumps(dict(passed=evidence['passed'], failed=[k for k, v in checks.items() if not v]), indent=2))
    return evidence['passed']


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    raise SystemExit(0 if verify(parser.parse_args().output.resolve()) else 1)
