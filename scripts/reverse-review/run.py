#!/usr/bin/env python3
"""Build the current tree and run the NAT matrix in disposable containers."""
import argparse
import concurrent.futures
import json
import hashlib
import datetime
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
    parser.add_argument('--transport', action='append',choices=['raw','tls','reality'])
    parser.add_argument('--cycles',type=int,default=1)
    parser.add_argument('--window',type=int,default=30)
    parser.add_argument('--rtt-ms',type=int,default=0)
    parser.add_argument('--race',action='store_true')
    parser.add_argument('--health-policy', type=Path, help='Complete reverse health policy JSON applied to both ends')
    parser.add_argument('--jobs',type=int,default=6)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    policy = json.loads(args.health_policy.read_text()) if args.health_policy else None
    if policy is not None:
        (args.output / "health-policy.json").write_text(json.dumps(policy, indent=2))
    image = 'brocade-reverse-review:' + uuid.uuid4().hex[:10]
    cases = [('flush_same', 90, {}), ('change_ip', 90, {}),
             ('change_ip_silent', args.silent_window, {}),
             ('downstream_silent', args.silent_window, {}),
             ('change_ip_silent', 300, {'LAB_KEEPALIVE': '5,2', 'LAB_RUN_SUFFIX': '-keepalive5-2'})]
    if args.case:
        cases = [case for case in cases if case[0] + case[2].get('LAB_RUN_SUFFIX', '') in args.case]
    cases = [(case,args.window,dict(env,LAB_TRANSPORT=transport,LAB_RTT_MS=str(args.rtt_ms),LAB_RUN_SUFFIX=env.get('LAB_RUN_SUFFIX','')+'-'+transport+'-'+str(cycle))) for cycle in range(args.cycles) for transport in (args.transport or ['raw']) for case,window,env in cases]
    with tempfile.TemporaryDirectory(prefix='brocade-reverse-review-') as tmp:
        tmp = Path(tmp)
        run('go', 'build', *(['-race'] if args.race else []), '-o', str(tmp / 'xray'), './main',
            cwd=ROOT / 'components/xray-core', env=dict(os.environ, CGO_ENABLED='1' if args.race else '0'))
        source=ROOT/'components/xray-core'
        manifest_files={str(p.relative_to(source)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(source.rglob('*.go'))}
        (args.output/'source-sha256.json').write_text(json.dumps(manifest_files,indent=2))
        manifest={'built_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'commit':sp.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'binary_sha256':hashlib.sha256((tmp/'xray').read_bytes()).hexdigest(),'go_version':sp.check_output(['go','version'],text=True).strip(),'arguments':vars(args)}
        (args.output/'manifest.json').write_text(json.dumps(manifest,default=str,indent=2))
        shutil.copy(tmp/'xray',args.output/'xray')
        shutil.copy(ROOT / 'scripts/reverse-nat-lab.py', tmp / 'reverse-nat-lab.py')
        setup=('FROM python:3.12-slim\nRUN apt-get update && apt-get install -y --no-install-recommends iproute2 iptables conntrack openssl procps && rm -rf /var/lib/apt/lists/*\n' if args.race else 'FROM python:3.12-alpine\nRUN apk add --no-cache iproute2 iproute2-tc iptables conntrack-tools openssl\n')
        (tmp / 'Dockerfile').write_text(setup+'COPY xray reverse-nat-lab.py /lab/\n')
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
                if policy is not None:
                    command += ['-e', 'LAB_REVERSE_HEALTH=' + json.dumps(policy)]
                for key, value in env.items():
                    command += ['-e', key + '=' + value]
                command += [name, 'python3', '/lab/reverse-nat-lab.py', case, str(window)]
                with (args.output / (label + '.log')).open('w') as log:
                    proc = sp.run(command, stdout=log, stderr=sp.STDOUT)
                run('docker', 'cp', name + ':/lab/results/' + label, str(args.output))
                for logfile in (args.output/label).glob('*.log'):
                    if 'DATA RACE' in logfile.read_text(errors='replace'):
                        raise RuntimeError(label+' data race detected')
                if proc.returncode:
                    raise RuntimeError(label + ' failed; see its log')
                print((args.output / label / 'summary.json').read_text(), flush=True)
            finally:
                sp.run(['docker', 'rm', '-f', name], stdout=sp.DEVNULL, stderr=sp.DEVNULL)

        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=min(max(1,args.jobs),len(cases))) as pool:
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
