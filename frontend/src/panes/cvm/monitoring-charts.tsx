import { type ReactNode } from 'react';
import type { CvmMonitoring, CvmMonitoringSample } from '../../api';
import { bytes } from '../../ui/format';
import { Section } from './presentation';
import { observeBpsReading, observeBpsUnit, observeBytesUnit, observeNumberUnit } from '../../ui/observe-chart';
import { cvmEventLabel, cvmNetworkRate } from './monitoring-data';
import { HistoryChart } from '../telemetry';

const pct = (n: number | null | undefined) => (n == null ? '—' : `${n.toFixed(1)}%`);
const num = (n: number | null | undefined, suffix = '') => (n == null ? '—' : `${Number(n.toFixed(2))}${suffix}`);
const size = (n: number | null | undefined) => (n == null ? '—' : bytes(n));
function Facts({ items }: { items: [string, ReactNode][] }) {
  return (
    <dl className="cvm-facts cvm-diag-facts">
      {items.map(([name, value]) => (
        <div key={name}>
          <dt>{name}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}
function Card({
  title,
  reading,
  children,
  more,
}: {
  title: string;
  reading: ReactNode;
  children: ReactNode;
  more: ReactNode;
}) {
  return (
    <Section title={title} className="cvm-diagnostic-card" aside={<span className="cvm-chart-reading">{reading}</span>}>
      {children}
      <details className="cvm-diagnostic-more">
        <summary>展开{title}诊断</summary>
        {more}
      </details>
    </Section>
  );
}
export default function CvmMonitoringCharts({
  data,
  nic,
  latest,
}: {
  data: CvmMonitoring;
  nic: string;
  latest: CvmMonitoringSample;
}) {
  const points = data.samples;
  const samples = points.map(s => ({
    window_start_unix_secs: (s.has_gap ? s.observed_at_unix_millis : s.window_start_unix_millis) / 1000,
    window_end_unix_secs: s.observed_at_unix_millis / 1000,
  }));
  const bounds = {
    rangeStartUnixSecs: data.range_start_unix_millis / 1000,
    rangeEndUnixSecs: data.range_end_unix_millis / 1000,
  };
  const events = data.events
    .slice(0, 40)
    .map(e => ({ atUnixSecs: e.at_unix_millis / 1000, label: cvmEventLabel(e.kind) }));
  const common = { samples, ...bounds, events };
  const diagnostics = latest.diagnostics;
  const cpu = diagnostics?.cpus.find(c => c.cpu === -1);
  const cpuSeries = (key: 'user' | 'system' | 'softirq' | 'iowait' | 'steal') =>
    points.map(s => s.diagnostics?.cpus.find(c => c.cpu === -1)?.[key] ?? null);
  const memory = (key: string) => diagnostics?.memory[key];
  const pressure = (resource: string) => {
    const p = diagnostics?.pressure.find(p => p.resource === resource);
    return p ? (
      <Facts
        items={[
          [`${resource} 等待任务 · 近 10 秒`, pct(p.some_basis_points / 100)],
          [
            `${resource} 全部任务等待 · 近 10 秒`,
            p.full_basis_points === null ? '不适用' : pct(p.full_basis_points / 100),
          ],
        ]}
      />
    ) : (
      <p className="note">该内核未提供此项压力数据。</p>
    );
  };
  const io = diagnostics?.disk_io;
  const selectedNics = latest.interfaces.filter(n => !nic || n.id === nic);
  const knownPeaks = points.map(s => s.cpu_peak_basis_points).filter((v): v is number => v !== null);
  const maxCpu = knownPeaks.length ? Math.max(...knownPeaks) / 100 : null;
  const memoryMinima = points.map(s => s.memory_available_min_bytes).filter((v): v is number => v !== null);
  return (
    <div className="cvm-diagnostic-grid">
      <Card
        title="CPU"
        reading={
          <>
            平均 <b>{pct(latest.cpu_usage_basis_points == null ? null : latest.cpu_usage_basis_points / 100)}</b>
          </>
        }
        more={
          <>
            <Facts
              items={[
                [
                  '最近窗口平均',
                  pct(latest.cpu_usage_basis_points == null ? null : latest.cpu_usage_basis_points / 100),
                ],
                ['所选区间采样峰值', points.length ? pct(maxCpu) : '—'],
              ]}
            />
            <HistoryChart
              {...common}
              title="CPU 时间分项"
              max={100}
              formatValue={n => pct(n)}
              lines={(['user', 'system', 'softirq', 'iowait', 'steal'] as const).map((key, i) => ({
                name: ['用户', '系统', '软中断', 'I/O 等待', 'Steal'][i],
                values: cpuSeries(key),
              }))}
            />
            <Facts
              items={[
                ['用户 / 系统 / 软中断', `${pct(cpu?.user)} / ${pct(cpu?.system)} / ${pct(cpu?.softirq)}`],
                ['I/O 等待 / Steal', `${pct(cpu?.iowait)} / ${pct(cpu?.steal)}`],
                ['Load 1 / 5 / 15', diagnostics?.load?.map(n => num(n)).join(' / ') ?? '—'],
                ['运行 / 总进程数', `${num(diagnostics?.running_processes)} / ${num(diagnostics?.total_processes)}`],
              ]}
            />
            {pressure('cpu')}
            {(diagnostics?.cpus.filter(c => c.cpu >= 0).length ?? 0) > 0 && (
              <div className="cvm-core-list">
                {diagnostics!.cpus
                  .filter(c => c.cpu >= 0)
                  .map(c => (
                    <div key={c.cpu}>
                      <span>CPU {c.cpu}</span>
                      <b>{pct(c.user + c.system + c.softirq)}</b>
                      <small>
                        I/O {pct(c.iowait)} · Steal {pct(c.steal)}
                      </small>
                    </div>
                  ))}
              </div>
            )}
          </>
        }
      >
        <HistoryChart
          {...common}
          title="CPU 使用率"
          max={100}
          formatValue={n => pct(n)}
          lines={[
            {
              name: '平均',
              values: points.map(s => (s.cpu_usage_basis_points === null ? null : s.cpu_usage_basis_points / 100)),
            },
            {
              name: '采样峰值',
              values: points.map(s => (s.cpu_peak_basis_points === null ? null : s.cpu_peak_basis_points / 100)),
              dashed: true,
            },
          ]}
        />
      </Card>
      <Card
        title="内存"
        reading={
          <>
            已用 <b>{bytes(latest.memory_used_bytes)}</b> / {bytes(latest.memory_total_bytes)}
          </>
        }
        more={
          <>
            <Facts
              items={[
                ['已用 / 可见总量', `${bytes(latest.memory_used_bytes)} / ${bytes(latest.memory_total_bytes)}`],
                ['所选区间最低可用', size(memoryMinima.length ? Math.min(...memoryMinima) : null)],
              ]}
            />
            <Facts
              items={[
                ['匿名内存', size(memory('AnonPages'))],
                ['文件缓存', size(memory('Cached'))],
                ['缓冲 / 可回收内核', `${size(memory('Buffers'))} / ${size(memory('SReclaimable'))}`],
                ['Swap 使用', size(diagnostics?.swap_used_bytes)],
                ['最近窗口 OOM', num(diagnostics?.oom_kills, ' 次')],
                ['重大缺页', num(diagnostics?.major_faults, ' 次')],
              ]}
            />
            {pressure('memory')}
            <HistoryChart
              {...common}
              title="Swap 使用"
              valueUnit={observeBytesUnit}
              lines={[{ name: 'Swap', values: points.map(s => s.diagnostics?.swap_used_bytes ?? null) }]}
            />
          </>
        }
      >
        <HistoryChart
          {...common}
          title="内存容量"
          valueUnit={observeBytesUnit}
          lines={[
            { name: '已用', values: points.map(s => s.memory_used_bytes) },
            { name: '可用', values: points.map(s => s.memory_total_bytes - s.memory_used_bytes) },
          ]}
        />
      </Card>
      <Card
        title="系统盘"
        reading={
          <>
            已用 <b>{bytes(latest.root_used_bytes)}</b> / {bytes(latest.root_total_bytes)}
          </>
        }
        more={
          <>
            <Facts
              items={[
                ['已用 / 文件系统总量', `${bytes(latest.root_used_bytes)} / ${bytes(latest.root_total_bytes)}`],
                ['可用空间', bytes(latest.root_total_bytes - latest.root_used_bytes)],
              ]}
            />
            <Facts
              items={[
                ['可用 inode / 总量', `${num(diagnostics?.inode_free)} / ${num(diagnostics?.inode_total)}`],
                ['读取 / 写入 IOPS', `${num(io?.read_iops)} / ${num(io?.write_iops)}`],
                ['读取 / 写入等待', `${num(io?.read_await_ms, ' ms')} / ${num(io?.write_await_ms, ' ms')}`],
                ['设备忙碌', pct(io?.busy_pct)],
                ['平均队列 / 正在执行', `${num(io?.queue_depth)} / ${num(io?.in_flight)}`],
              ]}
            />
            {!io && <p className="note">未取得根文件系统对应的独立块设备统计，I/O、IOPS 和延迟暂不可用。</p>}
            {pressure('io')}
            {io && (
              <HistoryChart
                {...common}
                title="磁盘等待时间"
                valueUnit={axis => observeNumberUnit(axis, 'ms')}
                lines={[
                  { name: '读等待', values: points.map(s => s.diagnostics?.disk_io?.read_await_ms ?? null) },
                  { name: '写等待', values: points.map(s => s.diagnostics?.disk_io?.write_await_ms ?? null) },
                ]}
              />
            )}
          </>
        }
      >
        {io ? (
          <HistoryChart
            {...common}
            title="磁盘 I/O"
            valueUnit={axis => observeBytesUnit(axis, '/s')}
            lines={[
              { name: '读取', values: points.map(s => s.diagnostics?.disk_io?.read_bps ?? null) },
              { name: '写入', values: points.map(s => s.diagnostics?.disk_io?.write_bps ?? null) },
            ]}
          />
        ) : (
          <>
            <HistoryChart
              {...common}
              title="根文件系统使用量"
              valueUnit={observeBytesUnit}
              lines={[{ name: '已用', values: points.map(s => s.root_used_bytes) }]}
            />
          </>
        )}
      </Card>
      <Card
        title="网络"
        reading={
          <>
            <span>
              接收{' '}
              <b>
                {cvmNetworkRate(latest, 'rx_bps', nic) === null
                  ? '—'
                  : observeBpsReading(cvmNetworkRate(latest, 'rx_bps', nic)!)}
              </b>
            </span>
            <span>
              发送{' '}
              <b>
                {cvmNetworkRate(latest, 'tx_bps', nic) === null
                  ? '—'
                  : observeBpsReading(cvmNetworkRate(latest, 'tx_bps', nic)!)}
              </b>
            </span>
          </>
        }
        more={
          <>
            {selectedNics.map(n => (
              <div key={n.id}>
                <h4>{n.device}</h4>
                <Facts
                  items={[
                    ['接收 / 发送包速率', `${num(n.rx_pps, ' 包/s')} / ${num(n.tx_pps, ' 包/s')}`],
                    ['窗口丢包 / 错误', `${num(n.drops)} / ${num(n.errors)}`],
                    ['链路', n.carrier === true ? '已就绪' : n.carrier === false ? '未就绪' : '未提供'],
                  ]}
                />
              </div>
            ))}
            <Facts
              items={[
                ['TCP 已建立连接', num(diagnostics?.tcp.CurrEstab)],
                ['窗口 TCP 重传', num(diagnostics?.tcp.RetransSegs, ' 段')],
                ['窗口连接失败 / 重置', `${num(diagnostics?.tcp.AttemptFails)} / ${num(diagnostics?.tcp.EstabResets)}`],
                ['窗口 UDP 接收错误', num(diagnostics?.tcp.UdpInErrors)],
              ]}
            />
            <p className="note">TCP／UDP 统计来自整个 Guest，按网卡选择不会改变这些全局计数。</p>
          </>
        }
      >
        <HistoryChart
          {...common}
          title="网卡流量"
          valueUnit={observeBpsUnit}
          lines={[
            { name: '接收', values: points.map(s => cvmNetworkRate(s, 'rx_bps', nic)) },
            { name: '发送', values: points.map(s => cvmNetworkRate(s, 'tx_bps', nic)) },
          ]}
        />
      </Card>
    </div>
  );
}
