#!/usr/bin/env python3
"""Independently audit retained results, including payload failures hidden by retries."""
import argparse
import hashlib
import json
from pathlib import Path


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(root):
    inputs = json.loads((root / 'inputs.json').read_text())
    rows = json.loads((root / 'summary.json').read_text())
    expected = {(iteration, case) for iteration in range(inputs['repeat']) for case in inputs['cases']}
    total = inputs['repeat'] * len(inputs['cases'])
    require(total > 0 and len(expected) == total == len(rows), 'missing, duplicate or empty case set')
    require(hashlib.sha256((root / 'agent').read_bytes()).hexdigest() == inputs['agent_sha256'], 'Agent identity changed')
    require(hashlib.sha256((root / 'runner.py').read_bytes()).hexdigest() == inputs['runner_sha256'], 'fixture identity changed')
    seen = set()
    requests = 0
    for row in rows:
        identity = (row['iteration'], row['case'])
        require(identity in expected and identity not in seen, 'unexpected or duplicate case')
        seen.add(identity)
        result = json.loads((root / f"{identity[0]:02d}-{identity[1]}" / 'result.json').read_text())
        require(row.get('exit_code') == 0 and row.get('passed') and result.get('passed'), f'{identity}: child or case failed')
        require(result.get('business_restored') and len(result['final_pids']) == 1, f'{identity}: no verified terminal business state')
        for sample in result['traffic']['long_streams']:
            require('corrupt' not in (sample.get('error') or ''), f'{identity}: corrupted established stream')
        for kind, samples in result['traffic']['raw'].items():
            for sample in samples:
                error = sample.get('error') or ''
                require('corrupt' not in error and 'sequence mismatch' not in error, f'{identity}: {kind} payload corruption')
            stats = result['traffic']['requests'][kind]
            require(len(samples) == stats['samples'] and sum(not sample['ok'] for sample in samples) == stats['failed'],
                    f'{identity}: {kind} request evidence disagrees with summary')
            require(samples and any(sample['ok'] for sample in samples), f'{identity}: no successful {kind} payloads')
            require(stats['max_success_gap_ms'] < 12000, f'{identity}: {kind} service gap exceeds bound')
            requests += len(samples)
    require(seen == expected, 'incomplete case set')
    return {'cases': len(rows), 'payload_requests': requests, 'verified': True}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('results', type=Path)
    args = parser.parse_args()
    print(json.dumps(verify(args.results), indent=2))
