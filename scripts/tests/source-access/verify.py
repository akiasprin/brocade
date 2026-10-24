#!/usr/bin/env python3
"""Independently check frozen inputs, real binary identities and the acceptance matrix."""
import argparse
import hashlib
import json
from pathlib import Path

CASES = {'distributed-binaries', 'initial-real-traffic', 'user-ban-isolates-ip-and-account',
         'scope-union-diagnostics', 'node-ban-and-scope-union', 'ipv6-isolation-and-unban',
         'offline-console-and-agent-restart-preserve-ban', 'api-conflict-and-validation', 'final-state'}


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
    require(manifest['runtime'] == 'docker' and result['passed'], 'Docker run did not complete')
    for name, field in [('console', 'console_sha256'), ('node.py', 'fixture_sha256'), ('support.py', 'support_sha256'),
                        ('run.py', 'runner_sha256'), ('geoip.dat', 'geoip_sha256'), ('geosite.dat', 'geosite_sha256')]:
        require(digest(root / 'inputs' / name) == manifest[field], 'frozen input changed: ' + name)
    isolation = json.loads((root / 'isolation.json').read_text())
    require(not isolation['privileged'] and isolation['network_internal'] and not isolation['published_ports']
            and isolation['network_mode'] != 'host' and isolation['pid_mode'] != 'host'
            and {cap.removeprefix('CAP_') for cap in isolation['cap_add']} == {'NET_ADMIN'}, 'invalid isolation')
    events = result['events']
    rows = {row['name']: row for row in events}
    require(len(events) == len(CASES) and set(rows) == CASES, 'missing or duplicated scenario')
    binary = rows['distributed-binaries']
    build = json.loads(args.xray_build_record.read_text())
    require(binary['xray_sha256'] == build['binary_sha256'], 'Xray differs from the selected verified build')
    require(binary['agent_sha256'] == digest(args.embedded_agent), 'Agent differs from the embedded build')
    initial = rows['initial-real-traffic']['xray_identity']
    require(len(initial) == 1 and initial[0]['pid'] > 0 and int(initial[0]['start_ticks']) > 0, 'invalid process identity')
    require(rows['offline-console-and-agent-restart-preserve-ban']['xray_identity'] == initial, 'policy operation restarted Xray')
    require(all(rows['scope-union-diagnostics'][key] for key in ['alice_blocked', 'bob_blocked', 'other_original_alive', 'other_new_allowed']), 'scope isolation failed')
    require(rows['user-ban-isolates-ip-and-account']['alice_closed'] and rows['user-ban-isolates-ip-and-account']['bob_samples'] >= 5, 'missing live user isolation')
    require(rows['node-ban-and-scope-union']['bob_closed'], 'node ban did not close the original carrier')
    require(rows['ipv6-isolation-and-unban']['ipv6_closed'] and rows['ipv6-isolation-and-unban']['ipv4_continues'], 'IPv6 isolation failed')
    require(all(rows['api-conflict-and-validation'][key] for key in ['conflict_rejected', 'invalid_rejected']), 'API validation failed')
    final = rows['final-state']
    require(final['migrations'] == [1, 2] and final['no_model_release'] and final['unchanged_xray_identity'] and final['original_flow_samples'] > 10, 'durability or release boundary failed')
    print(json.dumps({'passed': True, 'runtime': 'Docker', 'cases': len(CASES), **binary}))


if __name__ == '__main__':
    main()
