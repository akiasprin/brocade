#!/usr/bin/env python3
"""Independently verify fleet compression acceptance receipts and the exact frozen inputs."""
import argparse
import hashlib
import json
from pathlib import Path


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    manifest = json.loads((args.output / 'manifest.json').read_text())
    overall = json.loads((args.output / 'result.json').read_text())
    require(overall['passed'], 'the runner did not finish successfully')
    require(manifest['compression_required'] is True, 'compression was not required')
    for name, expected in manifest['inputs'].items():
        with (args.output / 'inputs' / name).open('rb') as stream:
            require(hashlib.file_digest(stream, 'sha256').hexdigest() == expected, 'frozen input changed: ' + name)
    summaries = []
    for runtime in overall['runtimes']:
        result = json.loads((args.output / runtime / 'result.json').read_text())
        require(result['passed'], runtime + ' did not pass')
        events = {item['name']: item for item in result['events']}
        require(len(events) == len(result['events']), 'duplicated case receipts')
        require(set(events) == {'environment', 'fleet-connected', 'forced-reconnect', 'large-sample-integrity', 'compressed-load', 'priority-rpc-wakes'}, 'missing or unexpected test receipts')
        environment = events['environment']
        require(environment['runtime'] == runtime, 'runtime receipt mismatch')
        require(environment['console_sha256'] == manifest['inputs']['console'] and environment['agent_sha256'] == manifest['inputs']['agent'], 'runtime binaries differ from the frozen inputs')
        connected = events['fleet-connected']
        require(connected['compression_required'] and connected['simulated'] == manifest['simulated_nodes'] and connected['real_agents'] == manifest['real_agents'], 'fleet was not fully connected with compression')
        wakes = events['priority-rpc-wakes']
        require(len(wakes['migrated_paths']) >= 7 and wakes['rpc_responses'] >= 7, 'priority RPC migration was not exercised')
        require({item['domain'] for item in wakes['receipts']} == {'agent', 'xray', 'source_access'}, 'update/policy wake receipts are missing')
        require(all(0 <= item['seconds'] < 4 for item in wakes['receipts']), 'update/policy wake exceeded four seconds')
        load = events['compressed-load']
        require(load['messages'] >= manifest['simulated_nodes'] * 4, 'load was empty or too short')
        require(load['duration_seconds'] >= manifest['seconds'], 'load duration was shortened')
        require(0 < load['websocket_data_wire_bytes'] < load['raw_payload_bytes'] * .5, 'compression savings were not measured')
        require(load['sse_messages'] >= manifest['simulated_nodes'], 'too few real Console events arrived')
        if manifest['seconds'] >= 60:
            require(load['sse_connections'] >= 2 and load['sse_authorization_refreshes'] >= 1, 'SSE reauthorization was not exercised')
        require(len(load['real_rpc_counts']) == manifest['real_agents'], 'real Agent RPC evidence is incomplete')
        if manifest['seconds'] >= 60:
            for node, counts in load['real_rpc_counts'].items():
                require(counts.get('POST /agent/v1/runtime', 0) > 0 and counts.get('POST /agent/v1/load', 0) > 0, 'small reports did not use RPC: ' + node)
        require(len(load['real_node_compression']) == manifest['real_agents'], 'real Agent evidence is incomplete')
        for node, evidence in load['real_node_compression'].items():
            require(evidence['connections'] >= 2 and evidence['up_compressed'] > 0 and evidence['down_compressed'] > 0, 'real Agent did not compress and reconnect: ' + node)
        large = events['large-sample-integrity']
        require(large['peers'] == min(16, manifest['simulated_nodes']) and large['rows_per_sample'] == 2048 and large['checked_rows'] == large['peers'] * 2048, 'large-message completeness check was skipped')
        require(200000 <= large['raw_bytes_min'] <= large['raw_bytes_max'] <= 512 * 1024, 'large messages were not actually large and bounded')
        if runtime == 'kvm':
            acceleration = json.loads((args.output / runtime / 'acceleration.json').read_text())
            require(acceleration.get('enabled') and acceleration.get('present'), 'KVM hardware acceleration was not proved')
            require(manifest['vm_sha256'], 'KVM image has no verified digest')
        summaries.append({'runtime': runtime, 'simulated_nodes': load['simulated_nodes'], 'real_agents': load['real_agents'],
                          'messages': load['messages'], 'savings_pct': load['savings_pct'],
                          'large_sample_bytes': large['raw_bytes_max'], 'console_rss_peak_kib': load['console_rss_peak_kib']})
    (args.output / 'verified.json').write_text(json.dumps({'passed': True, 'runtimes': summaries}, indent=2) + '\n')
    print(json.dumps({'passed': True, 'runtimes': summaries}))


if __name__ == '__main__':
    main()
