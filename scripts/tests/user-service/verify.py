#!/usr/bin/env python3
"""Independently verify Docker user-service acceptance evidence and frozen inputs."""
import argparse
import hashlib
import json
from pathlib import Path


CASES = {
    'environment', 'distributed-binaries', 'initial-full-deployment',
    'natural-expiry-revokes-existing-and-new-sessions', 'renewal-restores-both-services',
    'manual-disabled-grant-and-rule-remain-disabled', 'real-access-and-forwarding-quota-exhaustion',
    'reset-day-preview-preserves-real-ledgers', 'quota-recovery-does-not-bypass-expiry',
    'renewal-after-quota-recovery', 'scheduled-start-restores-real-traffic',
    'console-and-agent-restarts-preserve-expiry-and-other-user-sockets', 'final-state',
}


def require(value, message):
    if not value:
        raise SystemExit(message)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--xray-build-record', type=Path, required=True)
    parser.add_argument('--embedded-agent', type=Path, required=True)
    args = parser.parse_args()
    root = args.output
    manifest = json.loads((root / 'manifest.json').read_text())
    result = json.loads((root / 'results/result.json').read_text())
    require(result['passed'], 'container did not finish successfully')
    require(manifest['runtime'] == 'docker', 'unexpected runtime')
    for name, key in [('console', 'console_sha256'), ('node.py', 'fixture_sha256'), ('run.py', 'runner_sha256'),
                      ('geoip.dat', 'geoip_sha256'), ('geosite.dat', 'geosite_sha256')]:
        require(digest(root / 'inputs' / name) == manifest[key], f'frozen input changed: {name}')
    isolation = json.loads((root / 'isolation.json').read_text())
    require(not isolation['privileged'] and isolation['network_internal'] and not isolation['published_ports']
            and isolation['pid_mode'] != 'host' and isolation['network_mode'] != 'host'
            and {cap.removeprefix('CAP_') for cap in isolation['cap_add']} == {'NET_ADMIN'}, 'Docker isolation is invalid')
    events = result['events']
    require(len(events) == len(CASES), 'unexpected case count')
    rows = {row['name']: row for row in events}
    require(set(rows) == CASES, 'case matrix is incomplete or duplicated')
    environment = rows['environment']
    require(environment['runtime'] == 'docker' and environment['kernel'] and environment['boot_id']
            and 'Ubuntu' in environment['os_release'], 'container identity is missing')
    build = json.loads(args.xray_build_record.read_text())
    require(rows['distributed-binaries']['xray_sha256'] == build['binary_sha256'], 'container did not use the verified current Xray')
    require(rows['distributed-binaries']['agent_sha256'] == digest(args.embedded_agent), 'container did not use the selected embedded Agent')
    require(rows['initial-full-deployment']['status'] == 'succeeded', 'initial deployment did not succeed')
    detail = json.loads((root / 'results/initial-deployment.json').read_text())
    require(detail['status'] == 'succeeded' and detail['targets'] and all(
        target['status'] in ['succeeded', 'skipped'] for target in detail['targets']), 'initial receipt is incomplete')
    identity = rows['initial-full-deployment']['xray_identity']
    require(len(identity) == 1 and identity[0]['pid'] > 0 and int(identity[0]['start_ticks']) > 0, 'managed Xray identity is invalid')
    for row in events:
        if 'xray_identity' in row:
            require(row['xray_identity'] == identity, f'Xray restarted during {row["name"]}')
    expired = rows['natural-expiry-revokes-existing-and-new-sessions']
    require(8 <= expired['elapsed_secs'] <= 125 and expired['bob_samples'] > 5 and expired['bob_forward_samples'] > 5,
            'natural-expiration traffic evidence is invalid')
    quota = rows['real-access-and-forwarding-quota-exhaustion']
    require(quota['access_ledger_bytes'] >= 524288, 'real accounting did not reach the quota')
    preview = rows['reset-day-preview-preserves-real-ledgers']
    require(preview['access_used_bytes'] >= 524288 and preview['forwarding_used_bytes'] >= 524288,
            'reset-date preview lost booked consumption')
    restarted = rows['console-and-agent-restarts-preserve-expiry-and-other-user-sockets']
    require(restarted['bob_samples'] > expired['bob_samples'] and restarted['bob_forward_samples'] > expired['bob_forward_samples'],
            'unaffected connections stopped progressing')
    final = rows['final-state']
    require(final['migrations'] == [1, 2] and final['historical_ledger_preserved'] and final['other_user_errors'] == 0,
            'final durable state is invalid')
    print(json.dumps({'passed': True, 'runtime': 'Docker', 'cases': len(events), 'kernel': environment['kernel'],
                      'console_sha256': manifest['console_sha256'], **rows['distributed-binaries']}))


if __name__ == '__main__':
    main()
