#!/usr/bin/env python3
"""Independently audit raw evidence, including positive controls and actual paths."""
import argparse
import hashlib
import json
from pathlib import Path
import re


def udp_lines(text):
    return [line for line in text.splitlines() if re.search(r'(?:^|\s)udp\s+\d+\s',line)]


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('output',type=Path)
    parser.add_argument('--scope',choices=('all','baseline','prefix'),default='all')
    args=parser.parse_args();root=args.output;manifest=json.loads((root/'manifest.json').read_text())
    assert manifest.get('cleaned') is True
    if args.scope=='all':assert manifest.get('passed') is True
    assert len(manifest['query_kvm'])==4 and all(v=={'enabled':True,'present':True} for v in manifest['query_kvm'].values())
    assert len({v['boot_id'] for v in manifest['guests'].values()})==4
    for name,expected in manifest['artifacts'].items():
        with (root/'inputs'/name).open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==expected,name
    records=json.loads((root/'results'/'cases.json').read_text())
    if args.scope=='baseline':
        records=[r for r in records if r.get('profile')=='clean' or r['case']=='production-agent-start']
        expected=3+12*manifest['repeats']
    elif args.scope=='prefix':
        prefix=json.loads((root/'prefix-audit.json').read_text())
        assert prefix['passed'] and prefix['source_cases']==len(records)
        names=set(prefix.get('prefix_case_names',[r['case'] for r in records]))
        records=[r for r in records if r['case'] in names]
        expected=prefix.get('accepted_source_cases',prefix['source_cases'])
        assert expected>=10 and len(names)==expected
    else:
        app_expected=13 if manifest.get('app_reload_only') else 21
        expected={'network':134+12*manifest['repeats'],'combinations':132-18*manifest.get('start_profile',0),'firewalld':app_expected,'ufw':app_expected}[manifest['suite']]
        assert manifest['cases']==expected
    assert len(records)==expected
    assert all(r['passed'] for r in records)
    measured=[r for r in records if 'mode' in r];assert measured
    fq_samples=ifb_samples=namespace_samples=0
    hopping={}
    for record in measured:
        data=json.loads((root/'results'/(record['case']+'.json')).read_text())
        assert record['result']['passed'] and record['result']['verified'] and not record['result']['errors']
        assert sum(record['result']['bytes'].values())>0
        if record['mode']!='namespace':continue
        namespace_samples+=1
        node=data['after']['node'];family='ipv4' if record['ipv']==4 else 'ipv6'
        private=[line for line in udp_lines(node['hy2_ct']) if line.startswith(family)]
        assert private,record['case']+' missing family-specific namespace control'
        outer=[]
        for line in udp_lines(node['root_ct']):
            match=re.search(r'src=(\S+) dst=(\S+) sport=(\d+) dport=(\d+)',line)
            if not match:continue
            source,destination,sport,dport=match.groups()
            local={'198.18.2.2','fd42:bc:2::2','169.254.254.2','fd42:b10c:adf0::2'}
            if (destination in local and 30000<=int(dport)<=30199) or (source in local and 30000<=int(sport)<=30199):outer.append(line)
        assert not outer,record['case']+' root HY2 conntrack leak'
        assert not re.search(r'(?:^|\s)tcp\s+\d+\s',node['hy2_ct']), 'TCP outbound entered socket namespace'
        assert any(line.startswith(family) and 'dport=6001 ' in line and ' tcp ' in line for line in node['root_ct'].splitlines()), 'missing root TCP outbound positive control'
        if record['profile']!='clean':
            def pairs(text):
                return {tuple(re.search(r'src=(\S+) dst=(\S+) sport=(\d+) dport=(\d+)',line).groups())
                        for line in udp_lines(text) if line.startswith(family)}
            key=(record['profile'],record['ipv'])
            if key not in hopping:hopping[key]={'before':pairs(data['before']['node']['hy2_ct'])}
            hopping[key]['after']=pairs(node['hy2_ct'])
        for queue in node['qdisc']:
            if queue['kind']=='fq':
                assert queue.get('packets',0)>0, 'configured FQ carried no traffic'
                def check(value):
                    if isinstance(value,dict):
                        for key,item in value.items():
                            if key=='horizon_drops':assert item==0,'FQ horizon drops'
                            check(item)
                    elif isinstance(value,list):
                        for item in value:check(item)
                check(queue);fq_samples+=1
            if queue.get('dev')=='ifb-lab':
                assert queue.get('packets',0)>0,'configured IFB carried no traffic'
                ifb_samples+=1
    assert namespace_samples>0
    for (profile,version),entries in hopping.items():
        ports={int(row[3]) for row in entries['after']-entries['before']}
        first=30000 if version==4 else 30100
        assert len(ports)>=2 and all(first<=p<first+100 for p in ports),(profile,version,ports)
    if args.scope in ('baseline','prefix'):pass
    elif manifest['suite'] in ('network','combinations'):assert fq_samples>0 and ifb_samples>0
    else:
        reload=json.loads((root/'results'/'node'/'firewall-reload.json').read_text());assert reload['exit_code']==0
        attempts=reload.get('attempts',[reload])
        assert len(attempts)==manifest.get('reload_count',1) and all(item['exit_code']==0 for item in attempts),'incomplete reload series'
        live=next(r for r in records if r.get('profile')==manifest['suite']+'-live-reload')
        data=json.loads((root/'results'/(live['case']+'.json')).read_text())
        assert data['before']['node']['utc']<reload['started_at']<reload['ended_at']<data['after']['node']['utc'],'firewall reload did not overlap live payload'
    failures=list((root/'results').glob('**/*failure.txt'))
    if args.scope=='prefix':
        allowed=set(prefix.get('allowed_later_failure_paths',[]))
        assert all(str(p.relative_to(root)) in allowed for p in failures)
        if failures:
            reload_path=root/'results/node/firewall-reload.json'
            reload=json.loads(reload_path.read_text())
            last=max(json.loads((root/'results'/(r['case']+'.json')).read_text())['after']['node']['utc'] for r in measured)
            assert last<reload['started_at'],'excluded error predates the accepted measurements'
    else:assert not failures
    count=0
    if args.scope!='baseline':
        count=int((root/'results'/'node'/'docker-monitor-count.txt').read_text());assert count>=10
    observation_path=root/'results/node/docker-monitor-observations.json'
    observation=json.loads(observation_path.read_text()) if observation_path.exists() else None
    if observation and args.scope=='all':assert observation['last_request_passed'] is True,'Docker did not recover after reload'
    extra=root/'extra-observer.json'
    if extra.exists():
        extra=json.loads(extra.read_text());assert hashlib.sha256(Path(extra['path']).read_bytes()).hexdigest()==extra['sha256']
    result={'passed':True,'cases':len(records),'measurements':len(measured),'namespace_samples':namespace_samples,
            'fq_samples_with_traffic_and_zero_horizon_drops':fq_samples,'ifb_samples_with_traffic':ifb_samples,
            'concurrent_docker_egress_checks':count,'input_hashes_verified':True,'family_specific_conntrack_controls':True,
            'live_hopping_profile_families':len(hopping),'scope':args.scope,'full_run_passed':manifest.get('passed',False),
            'docker_egress_failed_requests':observation['failed_requests'] if observation else None,
            'docker_recoveries':observation.get('recoveries',[]) if observation else []}
    filename={'baseline':'baseline-audit.json','prefix':'prefix-evidence-audit.json','all':'evidence-audit.json'}[args.scope]
    (root/filename).write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))


if __name__=='__main__':main()
