#!/usr/bin/env python3
"""Build the current tree and run the NAT matrix in disposable containers."""
import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import subprocess as sp
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[2]


def run(*args, **kwargs):
    return sp.run(args, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/reverse-nat-review')
    parser.add_argument('--silent-window', type=int, default=660)
    parser.add_argument('--case', action='append', choices=[
        'flush_same', 'change_ip', 'change_ip_silent', 'downstream_silent',
        'change_ip_silent-keepalive5-2'], help='Run only selected cases; repeatable')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    image = 'brocade-reverse-review:' + uuid.uuid4().hex[:10]
    cases = [('flush_same', 90, {}), ('change_ip', 90, {}),
             ('change_ip_silent', args.silent_window, {}),
             ('downstream_silent', args.silent_window, {}),
             ('change_ip_silent', 300, {'LAB_KEEPALIVE': '5,2', 'LAB_RUN_SUFFIX': '-keepalive5-2'})]
    if args.case:
        cases = [case for case in cases if case[0] + case[2].get('LAB_RUN_SUFFIX', '') in args.case]
    with tempfile.TemporaryDirectory(prefix='brocade-reverse-review-') as tmp:
        tmp = Path(tmp)
        run('go', 'build', '-o', str(tmp / 'xray'), './main',
            cwd=ROOT / 'components/xray-core', env=dict(os.environ, CGO_ENABLED='0'))
        shutil.copy(ROOT / 'scripts/reverse-nat-lab.py', tmp / 'reverse-nat-lab.py')
        (tmp / 'Dockerfile').write_text('FROM python:3.12-alpine\n'
            'RUN apk add --no-cache iproute2 iptables conntrack-tools\n'
            'COPY xray reverse-nat-lab.py /lab/\n')
        run('docker', 'build', '-t', image, str(tmp))

        def experiment(spec):
            case, window, env = spec
            name = 'rvs-review-' + uuid.uuid4().hex[:10]
            label = case + env.get('LAB_RUN_SUFFIX', '')
            try:
                run('docker', 'run', '-d', '--name', name, '--network', 'none',
                    '--tmpfs', '/run', '--privileged', '--entrypoint', 'sleep', image, 'infinity',
                    stdout=sp.DEVNULL)
                command = ['docker', 'exec']
                for key, value in env.items():
                    command += ['-e', key + '=' + value]
                command += [name, 'python3', '/lab/reverse-nat-lab.py', case, str(window)]
                with (args.output / (label + '.log')).open('w') as log:
                    proc = sp.run(command, stdout=log, stderr=sp.STDOUT)
                run('docker', 'cp', name + ':/lab/results/' + label, str(args.output))
                if proc.returncode:
                    raise RuntimeError(label + ' failed; see its log')
                print((args.output / label / 'summary.json').read_text(), flush=True)
            finally:
                sp.run(['docker', 'rm', '-f', name], stdout=sp.DEVNULL, stderr=sp.DEVNULL)

        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=len(cases)) as pool:
                # Consume every result so a failed baseline or injection fails the run.
                futures = [pool.submit(experiment, case) for case in cases]
                failures = []
                for future in futures:
                    try:
                        future.result()
                    except Exception as exc:
                        failures.append(str(exc))
                if failures:
                    raise RuntimeError(json.dumps(failures))
        finally:
            sp.run(['docker', 'image', 'rm', image], stdout=sp.DEVNULL, stderr=sp.DEVNULL)


if __name__ == '__main__':
    main()
