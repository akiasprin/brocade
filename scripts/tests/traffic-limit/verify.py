#!/usr/bin/env python3
"""Independently verify retained acceptance evidence and frozen input identities."""
import argparse
import hashlib
import json
from pathlib import Path

EXPECTED = {
    'environment', 'uncapped-baseline', 'quota-percent-system-rx-tx',
    'system-connections-share-cap', 'system-duplex-independent-caps',
    'offline-absolute-strictest-rule', 'disable-cleans-owned-kernel-resources',
    'application-tcp-payload', 'application-scope-excludes-other-programs',
    'one-kbps-udp-long-wait', 'tc-failure-application-still-protects',
    'tc-failure-does-not-block-configuration', 'both-layers',
    'active-budget-outage-and-agent-restart',
    'budget-initialization-failure-retries-without-blocking-agent',
    'ready-for-real-reboot', 'offline-reboot-restores-both-layers', 'reboot-verified',
}
FAULTS = {
    'invalid-delivery-keeps-last-policy', 'truncated-delivery-keeps-last-policy',
    'stale-policy-keeps-active-cap', 'corrupt-policy-cache-recovers-authoritative-disable',
    'policy-fifo-does-not-stall-meter',
    'application-control-self-repair', 'corrupt-application-control-restart-recovers',
    'missing-control-startup-preserves-cap-until-meter-recovers',
    'corrupt-network-application-still-updates', 'corrupt-network-application-disable-and-recovery',
    'owned-qdisc-default-drift-self-heals', 'foreign-egress-survives-reconcile-and-disable',
    'foreign-root-preserved-application-still-protects',
    'foreign-ingress-preserved-application-still-protects',
    'tc-timeout-does-not-block-application',
}


def require(value, message):
    if not value:
        raise RuntimeError(message)


def verify(root):
    manifest = json.loads((root / 'manifest.json').read_text())
    expected = EXPECTED | FAULTS if manifest.get('fault_injection') else EXPECTED
    for name, key in [('agent', 'agent_sha256'), ('xray', 'xray_sha256'), ('guest.py', 'fixture_sha256')]:
        with (root / 'inputs' / name).open('rb') as source:
            require(hashlib.file_digest(source, 'sha256').hexdigest() == manifest[key],
                    f'frozen input changed: {name}')
    found = False
    for runtime in ['docker', 'kvm']:
        output = root / runtime
        if not output.exists():
            continue
        found = True
        require(not (output / 'failure.txt').exists(), f'{runtime}: retained failure')
        result = json.loads((output / 'result.json').read_text())
        rows = json.loads((output / 'evidence.json').read_text())
        require(result['passed'] is True and result['cases'] == len(rows), f'{runtime}: incomplete result')
        require({row['case'] for row in rows} == expected, f'{runtime}: incomplete case matrix')
        by_case = {row['case']: row for row in rows}
        if manifest.get('fault_injection'):
            meter = json.loads((output / 'policy-fifo-progress.json').read_text())
            require(meter['sequence_after'] >= meter['sequence_before'] + 2
                    and meter['rx_after'] > meter['rx_before'],
                    f'{runtime}: FIFO policy blocked meter commits or traffic accounting')
            repairs = by_case['application-control-self-repair']['recovery_seconds']
            require(set(repairs) == {'truncated', 'missing', 'oversized', 'fifo', 'symlink'}
                    and all(0 <= seconds < 30 for seconds in repairs.values()),
                    f'{runtime}: incomplete bounded/nonregular control recovery')
        require(by_case['uncapped-baseline']['measured_bps'] > 150_000_000, 'insufficient baseline')
        for row in rows:
            if 'target_bps' in row:
                rate = row['target_bps']
                require(set(row['measured_bps']) == {'D', 'U'}, f'{runtime}: missing direction')
                require(all(rate * .45 < value < rate * 1.35 for value in row['measured_bps'].values()),
                        f'{runtime}: invalid throughput: {row}')
        require(150_000_000 < by_case['system-duplex-independent-caps']['combined_bps'] < 270_000_000,
                f'{runtime}: duplex cap not verified')
        require(45_000_000 < by_case['system-connections-share-cap']['aggregate_bps'] < 135_000_000,
                f'{runtime}: connection sharing not verified')
        require(18 < by_case['one-kbps-udp-long-wait']['elapsed_seconds'] < 35,
                f'{runtime}: low-rate UDP not verified')
        receipt = by_case['tc-failure-does-not-block-configuration']
        require(receipt['system_error'] and receipt['observation']['result'] == 'applied'
                and receipt['observation']['error'] is None, f'{runtime}: config did not succeed')
        before = receipt['observation']['observed_before']['xray']['sha256']
        after = receipt['observation']['observed_after']['xray']['sha256']
        require(before != after and len(after) == 64, f'{runtime}: config was a no-op')
        reboot = by_case['reboot-verified']
        if runtime == 'kvm':
            require(reboot['old_boot_id'] != reboot['new_boot_id'], 'KVM did not really reboot')
        else:
            restart = json.loads((output / 'container-restart.json').read_text())
            require(restart['before'] != restart['after'], 'Docker did not really restart')
        print(json.dumps({'runtime': runtime, 'passed': True, 'evidence_rows': len(rows)}))
    require(found, 'no acceptance runtime found')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('results', type=Path)
    verify(parser.parse_args().results)
