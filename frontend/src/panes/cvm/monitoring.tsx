import { lazy, Suspense, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchCvmMonitoring, type CvmInstance } from '../../api';
import { navigate } from '../../forge/route';
import { Ago, ErrorBox, SegmentedControl } from '../../ui/bits';
import { useNow } from '../../ui/clock';
import { bytes } from '../../ui/format';
import { PanelLoading } from '../../ui/loading';
import { useMetricQuery } from '../../ui/metric-cadence';
import { observeBpsReading } from '../../ui/observe-chart';
import { cvmEventLabel, cvmNetworkRate } from './monitoring-data';
import { CardComet } from '../../ui/card-metrics';
import { observeSeriesData } from '../../ui/observe-series';
import { Section } from './presentation';
import { percent, resourceTone, shareOf } from './cards';
import { dur } from '../telemetry-format';

const Charts = lazy(() => import('./monitoring-charts'));
const healthLabel = (value?: string) =>
  ({
    running: '运行中',
    stopped: '已关机',
    missing: '未发现',
    transitioning: '变更中',
    reachable: '可连接',
    unreachable: '无响应',
    unknown: '待确认',
  })[value ?? 'unknown'] ?? '待确认';

const healthTone = (value: string) =>
  ['运行中', '可连接'].includes(value) || /^(\d+) \/ \1 可连接$/.test(value)
    ? 'ok'
    : ['无响应', '未发现'].includes(value) || / \/ /.test(value)
      ? 'err'
      : ['已关机', '未配置 TCP 探测'].includes(value)
        ? 'idle'
        : 'warn';

export function CvmMonitoringPanel({
  instance,
  compact = false,
  onOpen,
}: {
  instance: CvmInstance;
  compact?: boolean;
  onOpen?: () => void;
}) {
  const [seconds, setSeconds] = useState(3600);
  const query = useMetricQuery({
    queryKey: ['cvm-monitoring', instance.desired.id, seconds],
    queryFn: () => fetchCvmMonitoring(instance.desired.id, seconds),
    refetchInterval: 15_000,
    metricFacts: data => [data.state, data.error, ...data.issues.map(i => i.message)],
  });
  const now = useNow();
  const [nic, setNic] = useState('');
  const data = query.data;
  const latest = data?.latest;
  const health = data?.health;
  const stopped = health?.runtime_state === 'stopped';
  const serverNow = data ? data.range_end_unix_millis + Math.max(0, now - query.dataUpdatedAt) : now;
  const stale = !!latest && serverNow - latest.observed_at_unix_millis > 120_000;
  const healthStale = !!health && serverNow - health.observed_at_unix_millis > 120_000;
  const current = !query.error && !stopped && !stale && data?.state === 'live';
  const label = stopped
    ? '已关机'
    : query.error
      ? '读取失败'
      : stale || data?.state === 'stale'
        ? '读数已过期'
        : data?.state === 'live'
          ? '持续采样'
          : data?.error
            ? '采集失败'
            : '等待采样';
  const rate = (direction: 'rx_bps' | 'tx_bps') => (current ? cvmNetworkRate(latest, direction) : null);
  const nics = latest?.interfaces ?? [];
  const selected = nics.some(item => item.id === nic) ? nic : '';
  const healthFacts = [
    [
      '运行时',
      healthLabel(health?.runtime_state),
      instance.desired.spec.execution?.kind === 'baremetal' ? '宿主操作系统状态' : 'Cube 运行状态',
    ],
    [
      instance.desired.spec.execution?.kind === 'baremetal' ? '本机采样' : 'Guest 采样通道',
      healthLabel(health?.guest_state),
      instance.desired.spec.execution?.kind === 'baremetal' ? '宿主本机读取' : 'Guest 管理连接',
    ],
    ['SSH 端口', healthLabel(health?.ssh_state), '宿主 → 实例 TCP 22'],
    [
      '服务端口',
      !health?.services.length
        ? '未配置 TCP 探测'
        : `${health.services.filter(p => p.state === 'reachable').length} / ${health.services.length} 可连接`,
      '宿主 → 已配置 TCP 服务',
    ],
  ];
  const series = (pick: (sample: NonNullable<typeof data>['samples'][number]) => number | null) =>
    observeSeriesData(
      (data?.samples ?? []).map(s => s.observed_at_unix_millis / 1000),
      (data?.samples ?? []).map(s => (s.has_gap ? null : pick(s))),
      { windowStartsUnixSecs: (data?.samples ?? []).map(s => s.window_start_unix_millis / 1000) },
    ).map(([, value]) => value);
  const split = (text: string) => {
    const at = text.lastIndexOf(' ');
    return at < 0 ? [text, ''] : [text.slice(0, at), text.slice(at + 1)];
  };
  const native = instance.desired.spec.execution?.kind === 'baremetal';
  const issues = !!data?.issues?.length && (
    <ul className="cvm-monitoring-issues" aria-label="需要关注">
      {data.issues.map((issue, i) => (
        <li key={`${issue.kind}:${i}`} className={issue.severity === 'bad' ? 'error' : 'warn'}>
          {issue.message}
        </li>
      ))}
    </ul>
  );
  const notes = (
    <>
      {query.isPending && <PanelLoading label="读取实例监控" />}
      {query.error && <ErrorBox error={query.error} />}
      {data?.error && <p className="note">{data.error}</p>}
      {!query.isPending && !latest && !query.error && (
        <p className="note">
          {stopped ? '实例已关机，开机后继续采样。' : '尚无实例监控数据。宿主更新采集能力后会自动上报。'}
        </p>
      )}
    </>
  );
  if (compact) {
    const memoryShare = latest && current ? shareOf(latest.memory_used_bytes, latest.memory_total_bytes) : null;
    const diskShare = latest && current ? shareOf(latest.root_used_bytes, latest.root_total_bytes) : null;
    const tiles = latest
      ? [
          {
            title: 'CPU',
            note: `${instance.desired.spec.resources.vcpus} vCPU`,
            value:
              current && latest.cpu_usage_basis_points !== null
                ? [(latest.cpu_usage_basis_points / 100).toFixed(1), '%']
                : null,
            aside: null,
            share: current && latest.cpu_usage_basis_points !== null ? latest.cpu_usage_basis_points / 10_000 : null,
            values: series(s => (s.cpu_usage_basis_points === null ? null : s.cpu_usage_basis_points / 100)),
            floor: 20,
          },
          {
            title: '内存',
            note: `${bytes(latest.memory_total_bytes)} 可见`,
            value: current ? split(bytes(latest.memory_used_bytes)) : null,
            aside: percent(memoryShare),
            share: memoryShare,
            values: series(s => s.memory_used_bytes),
            floor: latest.memory_total_bytes,
          },
          {
            title: '系统盘',
            note: `文件系统 ${bytes(latest.root_total_bytes)}`,
            value: current ? split(bytes(latest.root_used_bytes)) : null,
            aside: percent(diskShare),
            share: diskShare,
            values: series(s => s.root_used_bytes),
            floor: latest.root_total_bytes,
          },
        ]
      : [];
    return (
      <div className="cvm-monitoring-view compact" role="region" aria-label="容器 监控">
        <header className="cvm-lane-head">
          <h2>运行</h2>
          <small>
            {latest ? (
              <>
                {label} · {current ? '采样于' : '最后采样于'}{' '}
                <Ago at={new Date(latest.observed_at_unix_millis).toISOString()} /> · 本次已运行{' '}
                {dur(latest.uptime_millis / 1000)}
              </>
            ) : (
              label
            )}
          </small>
          {onOpen && (
            <span className="cvm-lane-aside">
              <button className="cvm-text-link" onClick={onOpen}>
                监控与诊断 →
              </button>
            </span>
          )}
        </header>
        {notes}
        {latest && (
          <div className="cvm-stat-row" aria-label={current ? '当前实例读数' : '最后实例读数'}>
            {tiles.map(tile => (
              <div className="cvm-tile cvm-stat" key={tile.title}>
                <div className="cvm-stat-head">
                  <span>{tile.title}</span>
                  <small title={tile.note}>{tile.note}</small>
                </div>
                <div className="cvm-stat-value">
                  <b>{tile.value?.[0] ?? '—'}</b>
                  {tile.value && <u>{tile.value[1]}</u>}
                  {tile.aside && current && <small>{tile.aside}</small>}
                </div>
                <CardComet values={tile.values} floor={tile.floor} tone={resourceTone(tile.share)} current={current} />
              </div>
            ))}
            <div className="cvm-tile cvm-stat">
              <div className="cvm-stat-head">
                <span>网络</span>
                <small>{native ? '全部网卡 · 宿主视角' : '全部网卡 · Guest 视角'}</small>
              </div>
              <div className="cvm-stat-value cvm-rate-pair">
                {(['rx_bps', 'tx_bps'] as const).map(direction => {
                  const value = rate(direction);
                  const [number, unit] = value === null ? ['—', ''] : split(observeBpsReading(value));
                  return (
                    <span key={direction}>
                      <b>{number}</b>
                      <u>
                        {direction === 'rx_bps' ? '↓' : '↑'} {unit}
                      </u>
                    </span>
                  );
                })}
              </div>
              <CardComet
                values={series(s => cvmNetworkRate(s, 'rx_bps'))}
                floor={1_000_000}
                tone="ok"
                current={current}
              />
            </div>
          </div>
        )}
        {latest && current && latest.cpu_usage_basis_points === null && (
          <p className="note">CPU 和速率等待相邻两次有效采样。</p>
        )}
        {issues}
      </div>
    );
  }
  return (
    <div className="cvm-monitoring-view" role="region" aria-label="容器 监控">
      <div className="cvm-monitor-bar">
        <div className="cvm-monitoring-controls">
          <SegmentedControl
            ariaLabel="监控时间范围"
            value={seconds}
            options={[
              { value: 3600, label: '1 小时' },
              { value: 21600, label: '6 小时' },
              { value: 86400, label: '24 小时' },
            ]}
            onChange={setSeconds}
          />
          <select className="f" aria-label="监控网卡" value={selected} onChange={event => setNic(event.target.value)}>
            <option value="">全部网卡</option>
            {nics.map(item => (
              <option key={item.id} value={item.id}>
                {item.device} ·{' '}
                {instance.desired.spec.interfaces.find(n => n.id === item.id)?.purpose === 'contact'
                  ? '主网卡'
                  : '副网卡'}
              </option>
            ))}
          </select>
          <button className="btn small" onClick={() => navigate('nodes', { p: 'node', id: instance.node_id })}>
            宿主观测
          </button>
        </div>
        <ul className="cvm-health" aria-label="连接健康">
          <li className="cvm-health-sampling">
            <i className={`cg-lamp ${current ? 'ok' : stopped ? 'idle' : 'warn'}`} aria-hidden="true" />
            {label}
          </li>
          {healthFacts.map(([name, value, scope]) => (
            <li key={name} title={scope}>
              <i className={`cg-lamp ${healthStale ? 'warn' : healthTone(value)}`} aria-hidden="true" />
              {name} <b>{healthStale ? '读数已过期' : value}</b>
            </li>
          ))}
        </ul>
      </div>
      {notes}
      {issues}
      <details className="cvm-sampling-details">
        <summary>采样与探测说明</summary>
        <dl className="cvm-facts cvm-health-scopes">
          {healthFacts.map(([name, , scope]) => (
            <div key={name}>
              <dt>{name}</dt>
              <dd>{scope}</dd>
            </div>
          ))}
        </dl>
        <p className="note">
          {health && (
            <>
              健康观测于 <Ago at={new Date(health.observed_at_unix_millis).toISOString()} /> ·{' '}
            </>
          )}
          本地端口探测不代表公网链路可达；UDP 服务需单独拨测。
        </p>
        <p className="note">
          独立采样，不受配置任务是否成功影响。历史按分钟聚合，保留采样峰值和最低可用内存。缺失与重启区间断开，未知值显示为
          —。
        </p>
      </details>
      {data?.samples_truncated && <p className="note">所选区间超过展示上限，只显示最近的历史记录。</p>}
      {latest && data && (
        <Suspense fallback={<PanelLoading label="加载监控曲线" />}>
          <Charts data={data} nic={selected} latest={latest} />
        </Suspense>
      )}
      <Section
        title="事件与操作"
        meta={seconds === 3600 ? '近 1 小时' : seconds === 21600 ? '近 6 小时' : '近 24 小时'}
      >
        {!data?.events?.length ? (
          <p className="note">所选时间内没有事件记录。</p>
        ) : (
          <EventList events={data.events} native={native} full />
        )}
      </Section>
    </div>
  );
}

const eventTone = (kind: string) =>
  kind === 'oom'
    ? 'err'
    : ['reboot', 'guest_unreachable', 'service_unreachable'].includes(kind)
      ? 'warn'
      : kind.endsWith('_recovered')
        ? 'ok'
        : 'idle';

function EventList({
  events,
  native,
  full = false,
}: {
  events: { at_unix_millis: number; kind: string; detail: string }[];
  native: boolean;
  full?: boolean;
}) {
  return (
    <ol className="cvm-monitoring-events">
      {events.map((event, index) => (
        <li key={`${event.at_unix_millis}:${event.kind}:${index}`}>
          <i className={`cg-lamp ${eventTone(event.kind)}`} aria-hidden="true" />
          <strong>{cvmEventLabel(event.kind, native)}</strong>
          <time dateTime={new Date(event.at_unix_millis).toISOString()}>
            {new Date(event.at_unix_millis).toLocaleString(
              'zh-CN',
              full ? undefined : { dateStyle: 'short', timeStyle: 'short' },
            )}
          </time>
          {event.detail && <span>{event.detail}</span>}
        </li>
      ))}
    </ol>
  );
}

export function CvmRecentEvents({ id, onOpen, native = false }: { id: string; onOpen?: () => void; native?: boolean }) {
  const query = useQuery({
    queryKey: ['cvm-monitoring', id, 3600],
    queryFn: () => fetchCvmMonitoring(id),
    staleTime: 15_000,
  });
  return (
    <Section
      title="最近事件"
      meta="近 1 小时"
      className="cvm-recent-events"
      aside={
        onOpen && (
          <button className="cvm-text-link" onClick={onOpen}>
            全部 →
          </button>
        )
      }
    >
      {query.error && <ErrorBox error={query.error} />}
      {!query.isPending && !query.error && !query.data?.events.length && (
        <p className="note">近 1 小时没有事件记录。</p>
      )}
      {!!query.data?.events.length && <EventList events={query.data.events.slice(0, 4)} native={native} />}
    </Section>
  );
}
