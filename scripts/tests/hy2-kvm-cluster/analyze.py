#!/usr/bin/env python3
"""Summarize recorded receiver-verified measurements without inventing thresholds."""
import argparse
import csv
import json
from pathlib import Path
import statistics


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output',type=Path)
    parser.add_argument('--scope',choices=('all','baseline','prefix'),default='all')
    args=parser.parse_args();root=args.output
    manifest=json.loads((root/'manifest.json').read_text())
    records=json.loads((root/'results'/'cases.json').read_text())
    if args.scope=='baseline':records=[r for r in records if r.get('profile')=='clean' or r['case']=='production-agent-start']
    if args.scope=='prefix':
        names=set(json.loads((root/'prefix-audit.json').read_text()).get('prefix_case_names',[r['case'] for r in records]))
        records=[r for r in records if r['case'] in names]
    measured=[r for r in records if 'mode' in r]
    groups={}
    for record in measured:
        key=(record['profile'],record['mode'],record['ipv'],record['direction'],record['streams'])
        groups.setdefault(key,[]).append(record)
    summary=[]
    for key,items in groups.items():
        total=[sum(r['result']['mbps'].values()) for r in items]
        name='managed-xray' if key[1]=='namespace' else 'baseline'
        cpus=[r['cpu_percent_one_core_100']['node'].get(name,0) for r in items]
        linux_cpu=[]
        for record in items:
            raw=json.loads((root/'results'/(record['case']+'.json')).read_text())
            before=raw['before']['node'];after=raw['after']['node']
            start=list(map(int,before['stat'].splitlines()[0].split()[1:]));end=list(map(int,after['stat'].splitlines()[0].split()[1:]))
            busy=sum(end[i]-start[i] for i in (0,1,2,5,6))
            linux_cpu.append(busy/after['clock_ticks']/(after['monotonic']-before['monotonic'])*100)
        summary.append({'profile':key[0],'mode':key[1],'ipv':key[2],'direction':key[3],'streams':key[4],
                        'n':len(items),'median_mbps':statistics.median(total),'min_mbps':min(total),'max_mbps':max(total),
                        'median_node_xray_cpu_percent':statistics.median(cpus),
                        'median_node_linux_busy_cpu_percent':statistics.median(linux_cpu),
                        'median_node_hypervisor_cpu_percent':statistics.median(r['hypervisor_cpu_percent']['node'] for r in items)})
    comparisons=[]
    for row in summary:
        if row['profile']=='clean' and row['mode']=='namespace':
            baseline=next(r for r in summary if r['profile']=='clean' and r['mode']=='root-hy2' and all(r[k]==row[k] for k in ('ipv','direction','streams')))
            comparisons.append({'ipv':row['ipv'],'direction':row['direction'],'streams':row['streams'],
                                'root_mbps':baseline['median_mbps'],'namespace_mbps':row['median_mbps'],
                                'namespace_vs_root_percent':(row['median_mbps']/baseline['median_mbps']-1)*100,
                                'root_cpu_percent':baseline['median_node_xray_cpu_percent'],
                                'namespace_cpu_percent':row['median_node_xray_cpu_percent']})
    report={'passed':manifest.get('passed',False),'cases':len(records),'measurements':len(measured),
            'summary':summary,'comparisons':comparisons,'started_at':manifest['started_at'],'ended_at':manifest.get('ended_at'),
            'elapsed_seconds':manifest.get('elapsed_seconds'),'artifacts':manifest['artifacts']}
    if args.scope=='baseline':
        report['passed']=json.loads((root/'baseline-audit.json').read_text())['passed']
        report['scope']='completed baseline; full source run failed later at Docker setup'
    if args.scope=='prefix':
        report['passed']=json.loads((root/'prefix-evidence-audit.json').read_text())['passed']
        report['scope']=json.loads((root/'prefix-audit.json').read_text())['scope']
    filename={'all':'summary.json','baseline':'summary-baseline.json','prefix':'summary-prefix.json'}[args.scope]
    (root/filename).write_text(json.dumps(report,indent=2)+'\n')
    with (root/'measurements.csv').open('w') as f:
        fields=['profile','mode','ipv','direction','streams','repetition','elapsed_seconds','download_mbps','upload_mbps','download_bytes','upload_bytes','node_xray_cpu_percent','node_hypervisor_cpu_percent']
        writer=csv.DictWriter(f,fieldnames=fields);writer.writeheader()
        for r in measured:
            result=r['result'];name='managed-xray' if r['mode']=='namespace' else 'baseline'
            writer.writerow({**{k:r[k] for k in fields[:6]},'elapsed_seconds':result['elapsed_seconds'],
                             'download_mbps':result['mbps']['download'],'upload_mbps':result['mbps']['upload'],
                             'download_bytes':result['bytes']['download'],'upload_bytes':result['bytes']['upload'],
                             'node_xray_cpu_percent':r['cpu_percent_one_core_100']['node'].get(name,0),
                             'node_hypervisor_cpu_percent':r['hypervisor_cpu_percent']['node']})
    lines=['# HY2 四客体 KVM 实测', '', f"结果：{'通过' if report['passed'] else '未完成或失败'}，{len(records)} 项检查，{len(measured)} 组接收端校验吞吐测量。",'',
           f"开始 `{report['started_at']}`，结束 `{report['ended_at']}`，单调时钟耗时 {report['elapsed_seconds']} 秒。",'',
           '## 清洁网络，同版本 Xray 对照（三次中位数）','',
           '| IP | 流量 | 每方向流数 | 主空间 Mbps | 专用空间 Mbps | 差异 | Xray CPU 主/专用 |',
           '|---|---|---:|---:|---:|---:|---:|']
    if args.scope=='baseline':lines.insert(2,'本报告仅确认已完成的基准部分；原始整轮在后续 Docker 夹具初始化失败，原 manifest 保留失败状态，组合部分由独立新集群验证。')
    if args.scope=='prefix':lines.insert(2,'本报告仅确认已完成并独立审计的组合前缀；原始整轮保留失败状态，未完成的后续组合另行验证。')
    for r in comparisons:
        lines.append(f"| v{r['ipv']} | {r['direction']} | {r['streams']} | {r['root_mbps']:.1f} | {r['namespace_mbps']:.1f} | {r['namespace_vs_root_percent']:+.1f}% | {r['root_cpu_percent']:.1f}% / {r['namespace_cpu_percent']:.1f}% |")
    lines+=['','双向 Mbps 为上行与下行之和；CPU 的 100% 表示一个逻辑核。每次测量的最小值、最大值和原始计数见 summary.json 与 measurements.csv。', '',
            '## 累加第三方网络组合','', '| 组合 | IP | 流数/方向 | 次数 | 双向 Mbps 中位数 | 范围 | 节点 Xray CPU | 节点 QEMU CPU |',
            '|---|---|---:|---:|---:|---:|---:|---:|']
    for r in summary:
        if r['profile']=='clean':continue
        lines.append(f"| {r['profile']} | v{r['ipv']} | {r['streams']} | {r['n']} | {r['median_mbps']:.1f} | {r['min_mbps']:.1f}–{r['max_mbps']:.1f} | {r['median_node_xray_cpu_percent']:.1f}% | {r['median_node_hypervisor_cpu_percent']:.1f}% |")
    lines+=['', '## 边界', '',
            '客户端、路由器、节点和目标各自具有独立内核，全部 QMP 验证 KVM；虚拟机共享同一物理宿主。没有云网卡、物理交换机或 ARM 性能结论。',
            '主空间与独立空间使用完全相同的正式 Xray 字节；专用空间由正式 Agent 创建，使用动态授权。对照差异包括空间转发、NAT 和规则处理，不能全部归因于 BPF。',
            '每个数据帧校验内容并核对接收端确认字节数；组合网络使用真实 Docker、真实 nft/legacy 和真实 TC 操作。',
            '逐项结果、前后 conntrack/队列/CPU、客体命令与回复、串口日志、镜像与输入摘要保留在本目录。失败尝试在相邻目录单独保留。', '']
    filename={'all':'REPORT.md','baseline':'BASELINE.md','prefix':'PREFIX.md'}[args.scope]
    (root/filename).write_text('\n'.join(lines))
    print(json.dumps({'passed':report['passed'],'cases':len(records),'measurements':len(measured),'comparisons':comparisons},ensure_ascii=False))


if __name__=='__main__':main()
