#!/usr/bin/env python3
"""Recompute recovery from raw records, excluding probes overlapping injection."""
import datetime
import json
from pathlib import Path
import sys


def summarize(path, clock_offset=None):
    fault = json.loads((path / 'fault.json').read_text())
    t0 = fault['completed']
    rows = [json.loads(line) for line in (path / 'probes.jsonl').read_text().splitlines()]
    rows = [r for r in rows if r['t'] >= t0]
    failures = [r for r in rows if not r['ok']]
    good = [r for r in rows if r['ok']]
    last_failure_end = max((r['t'] + r['latency'] for r in failures), default=t0)
    tail = [r for r in good if r['t'] >= last_failure_end]
    result = dict(case=path.name, attempts=len(rows), successes=len(good),
                  observed_s=max((r['t']+r['latency']-t0 for r in rows), default=0),
                  first_success_s=good[0]['t']+good[0]['latency']-t0 if good else None,
                  last_failure_s=last_failure_end-t0 if failures else None,
                  stable_recovery_s=tail[0]['t']+tail[0]['latency']-t0 if len(tail) >= 20 else None,
                  stable_successes=len(tail))
    # Receipt of the reverse control stream verifies a newly authenticated tunnel,
    # rather than merely a TCP dial attempt. Legacy runs use a measured clock offset.
    if 'wall_completed' in fault:
        wall_t0 = fault['wall_completed']
    elif clock_offset is not None:
        wall_t0 = t0 + clock_offset
    else:
        wall_t0 = None
    if wall_t0 is not None:
        ready = []
        for line in (path / 'b.log').read_text().splitlines():
            if 'received request for udp:reverse:0' in line:
                stamp = datetime.datetime.strptime(line[:26], '%Y/%m/%d %H:%M:%S.%f')
                seconds = stamp.replace(tzinfo=datetime.timezone.utc).timestamp() - wall_t0
                if seconds >= 0:
                    ready.append(seconds)
        result['new_tunnel_ready_s'] = ready[0] if ready else None
    return result


if __name__ == '__main__':
    root = Path(sys.argv[1])
    env = root / 'environment.json'
    offset = json.loads(env.read_text()).get('wall_minus_monotonic') if env.exists() else None
    summaries = [summarize(p.parent, offset) for p in sorted(root.rglob('fault.json'))]
    print(json.dumps(summaries, indent=2))
