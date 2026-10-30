#!/usr/bin/env python3
"""Verify real Docker continuity evidence and the selected immutable build inputs."""
import argparse
import hashlib
import json
from pathlib import Path


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
    require(result['passed'], 'continuity acceptance failed; see the private result and failure files')
    require(manifest['runtime'] == 'docker' and manifest['node_arguments'] == [result['mode']], 'runtime/mode mismatch')
    for filename, field in [('console', 'console_sha256'), ('node.py', 'fixture_sha256'),
                            ('support.py', 'support_sha256'), ('run.py', 'runner_sha256'),
                            ('geoip.dat', 'geoip_sha256'), ('geosite.dat', 'geosite_sha256')]:
        require(digest(root / 'inputs' / filename) == manifest[field], 'changed frozen input: ' + filename)
    isolation = json.loads((root / 'isolation.json').read_text())
    require(not isolation['privileged'] and isolation['network_internal'] and not isolation['published_ports']
            and isolation['network_mode'] != 'host' and isolation['pid_mode'] != 'host'
            and {cap.removeprefix('CAP_') for cap in isolation['cap_add']} == {'NET_ADMIN'}, 'invalid Docker isolation')
    rows = {event['name']: event for event in result['events']}
    cases = {'environment', 'distributed-binaries', 'initial-full-deployment', 'live-request-baseline',
             'draft-and-commit-preserve-live-requests', 'real-publication-traffic', 'real-billing-switch'}
    require(set(rows) == cases and len(result['events']) == len(cases), 'missing/duplicate acceptance case')
    baseline = rows['live-request-baseline']
    require(len(baseline['tcp_flows']) == 4 and all(flow['samples'] >= 5 and flow['error'] is None
            for flow in baseline['tcp_flows'].values()) and baseline['long_samples'] >= 5
            and baseline['long_error'] is None and not baseline['short_errors'], 'invalid live baseline')
    binaries = rows['distributed-binaries']
    xray = json.loads(args.xray_build_record.read_text())
    require(binaries['xray_sha256'] == xray['binary_sha256'], 'Xray differs from selected build')
    require(binaries['agent_sha256'] == digest(args.embedded_agent), 'Agent differs from selected build')
    traffic = rows['real-publication-traffic']
    plan = json.loads((root / 'results/move-plan.json').read_text())
    deployment = json.loads((root / 'results/move-deployment.json').read_text())
    require(plan['summary']['changed_targets'] > 0 and any('apply-xray' in target['actions']
            for target in plan['targets']), 'test publication did not change a real Xray target')
    require(deployment['status'] == 'succeeded' and deployment['targets'] and all(
            target['status'] in ['succeeded', 'skipped'] for target in deployment['targets']), 'publication did not converge')
    require(len(traffic['tcp_flows']) == 4 and all(flow['error'] is None and flow['after'] > flow['before']
            and flow['connect_calls'] == 1 for flow in traffic['tcp_flows'].values()), 'TCP continuity failed')
    long = traffic['long_http']
    require(long['chunks'] == long['expected_chunks'] == 800 and long['connect_calls'] == 1
            and long['error'] is None and long['spanned_publication'], 'long HTTP request did not survive publication')
    short = traffic['new_http']
    require(not short['errors'] and short['success'] > short['before'], 'new HTTP request failed')
    billing = rows['real-billing-switch']
    require(billing['old_after'] >= billing['old_before'] > 0 and billing['new_after'] > 0, 'billing ownership failed')
    print(json.dumps({'passed': True, 'mode': result['mode'], 'cases': len(cases),
                      'long_http_chunks': long['chunks'], 'http_requests': short['success'], **binaries}))


if __name__ == '__main__':
    main()
