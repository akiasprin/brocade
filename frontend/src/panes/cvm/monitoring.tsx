import { lazy, Suspense, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchCvmMonitoring, type CvmInstance } from '../../api';
import { navigate } from '../../forge/route';
import { Ago, ErrorBox, SegmentedControl } from '../../ui/bits';
import { useNow } from '../../ui/clock';
import { bytes } from '../../ui/format';
import { PanelTitle } from '../../ui/icons';
import { PanelLoading } from '../../ui/loading';
import { useMetricQuery } from '../../ui/metric-cadence';
import { observeBpsReading } from '../../ui/observe-chart';
import { cvmEventLabel, cvmNetworkRate } from './monitoring-data';
import { CardComet, CardRing } from '../../ui/card-metrics';
import { observeSeriesData } from '../../ui/observe-series';
import { Section } from './presentation';
import { percent, resourceTone, shareOf } from './cards';
import { dur } from '../telemetry-format';

const Charts = lazy(() => import('./monitoring-charts'));
const reading = (n: number | null | undefined) => (n == null ? '—' : `${(n / 100).toFixed(1)}%`);
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
    ['运行时', healthLabel(health?.runtime_state), 'Cube 运行状态'],
    ['Guest 采样通道', healthLabel(health?.guest_state), 'Guest 管理连接'],
    ['SSH 端口', healthLabel(health?.ssh_state), '宿主 → 实例 TCP 22'],
    [
      '服务端口',
      !health?.services.length
        ? '未配置 TCP 探测'
        : `${health.services.filter(p => p.state === 'reachable').length} / ${health.services.length} 可连接`,
      '宿主 → 已配置 TCP 服务',
    ],
  ];
  return (
    <div className="cvm-monitoring-view" role="region" aria-label="容器 监控">
      <section className={`panel titled cvm-monitoring${compact ? ' compact' : ''}`}>
        <header>
          <PanelTitle of="usage">{compact ? '运行' : '监控与诊断'}</PanelTitle>
          <span className="note">{label}</span>
          {compact && onOpen && (
            <button className="cvm-text-link" onClick={onOpen}>
              查看监控与诊断
            </button>
          )}
        </header>
        <div className="cvm-section-body">
          {query.isPending && <PanelLoading label="读取实例监控" />}
          {query.error && <ErrorBox error={query.error} />}
          {data?.error && <p className="note">{data.error}</p>}
          {!query.isPending && !latest && !query.error && (
            <p className="note">
              {stopped ? '实例已关机，开机后继续采样。' : '尚无实例监控数据。宿主更新采集能力后会自动上报。'}
            </p>
          )}
          {latest && compact && (
            <>
              <div className="cvm-monitoring-kpis" aria-label={current ? '当前实例读数' : '最后实例读数'}>
                {[
                  {
                    title: 'CPU 使用率',
                    share:
                      current && latest.cpu_usage_basis_points !== null ? latest.cpu_usage_basis_points / 10000 : null,
                    value: current ? reading(latest.cpu_usage_basis_points) : '—',
                    note: `${instance.desired.spec.resources.vcpus} vCPU`,
                  },
                  {
                    title: '内存使用',
                    share: current ? shareOf(latest.memory_used_bytes, latest.memory_total_bytes) : null,
                    value: current ? bytes(latest.memory_used_bytes) : '—',
                    note: `/ ${bytes(latest.memory_total_bytes)} 可见内存`,
                  },
                  {
                    title: '系统盘使用',
                    share: current ? shareOf(latest.root_used_bytes, latest.root_total_bytes) : null,
                    value: current ? bytes(latest.root_used_bytes) : '—',
                    note: `/ ${bytes(latest.root_total_bytes)} 文件系统`,
                  },
                  {
                    title: '接收 / 发送',
                    share: undefined,
                    value: (
                      <span className="cvm-rate-pair">
                        <span>{rate('rx_bps') === null ? '—' : observeBpsReading(rate('rx_bps')!)}</span>
                        <span aria-hidden="true">{' / '}</span>
                        <span>{rate('tx_bps') === null ? '—' : observeBpsReading(rate('tx_bps')!)}</span>
                      </span>
                    ),
                    note: '全部已挂载网卡 · Guest 视角',
                  },
                ].map(({ title, value, note, share }) => (
                  <div key={title}>
                    {compact && share !== undefined && (
                      <div className="cvm-gauge">
                        <CardRing share={share} tone={resourceTone(share)} size={58} />
                        <b>{percent(share)}</b>
                      </div>
                    )}
                    <div className="cvm-reading-text">
                      <span>{title}</span>
                      {(!compact || title !== 'CPU 使用率') && <strong>{value}</strong>}
                      <small title={note}>{note}</small>
                    </div>
                  </div>
                ))}
              </div>
              {compact && (
                <CardComet
                  values={observeSeriesData(
                    (data?.samples ?? []).map(s => s.observed_at_unix_millis / 1000),
                    (data?.samples ?? []).map(s =>
                      s.has_gap || s.cpu_usage_basis_points === null ? null : s.cpu_usage_basis_points / 100,
                    ),
                    { windowStartsUnixSecs: (data?.samples ?? []).map(s => s.window_start_unix_millis / 1000) },
                  ).map(([, value]) => value)}
                  floor={20}
                  tone="ok"
                  current={current}
                />
              )}
              <p className="note cvm-trend-caption">
                <span>近 1 小时 CPU</span>
                <span>
                  {current ? '采样于' : '最后采样于'}{' '}
                  <Ago at={new Date(latest.observed_at_unix_millis).toISOString()} /> · 该次采样已运行{' '}
                  {dur(latest.uptime_millis / 1000)}
                  {latest.cpu_usage_basis_points === null && current ? ' · CPU 和速率等待相邻两次有效采样' : ''}
                </span>
              </p>
            </>
          )}
          {!!data?.issues?.length && (
            <ul className="cvm-monitoring-issues" aria-label="需要关注">
              {data.issues.map((issue, i) => (
                <li key={`${issue.kind}:${i}`} className={issue.severity === 'bad' ? 'error' : 'warn'}>
                  {issue.message}
                </li>
              ))}
            </ul>
          )}
          {!compact && (
            <>
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
                  <label>
                    网卡{' '}
                    <select
                      className="f"
                      aria-label="监控网卡"
                      value={selected}
                      onChange={event => setNic(event.target.value)}
                    >
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
                  </label>
                  <button className="btn small" onClick={() => navigate('nodes', { p: 'node', id: instance.node_id })}>
                    宿主观测
                  </button>
                </div>
                <dl className="cvm-facts cvm-health-facts" aria-label="连接健康">
                  {healthFacts.map(([name, value, scope]) => (
                    <div key={name}>
                      <dt>{name}</dt>
                      <dd title={scope}>{healthStale ? '读数已过期' : value}</dd>
                    </div>
                  ))}
                </dl>
              </div>
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
            </>
          )}
        </div>
      </section>
      {!compact && latest && data && (
        <Suspense fallback={<PanelLoading label="加载监控曲线" />}>
          <Charts data={data} nic={selected} latest={latest} />
        </Suspense>
      )}
      {!compact && (
        <Section title="事件与操作">
          {!data?.events?.length ? (
            <p className="note">所选时间内没有事件记录。</p>
          ) : (
            <ol className="cvm-monitoring-events">
              {data.events.map((e, i) => (
                <li key={`${e.at_unix_millis}:${e.kind}:${i}`}>
                  <time dateTime={new Date(e.at_unix_millis).toISOString()}>
                    {new Date(e.at_unix_millis).toLocaleString('zh-CN')}
                  </time>
                  <strong>{cvmEventLabel(e.kind)}</strong>
                  <span>{e.detail}</span>
                </li>
              ))}
            </ol>
          )}
        </Section>
      )}
    </div>
  );
}

export function CvmRecentEvents({ id, onOpen }: { id: string; onOpen?: () => void }) {
  const query = useQuery({
    queryKey: ['cvm-monitoring', id, 3600],
    queryFn: () => fetchCvmMonitoring(id),
    staleTime: 15_000,
  });
  return (
    <Section
      title="最近事件"
      className="cvm-widget-12"
      aside={
        onOpen && (
          <button className="cvm-text-link" onClick={onOpen}>
            监控 →
          </button>
        )
      }
    >
      {query.error && <ErrorBox error={query.error} />}
      {!query.isPending && !query.error && !query.data?.events.length && (
        <p className="note">近 1 小时没有事件记录。</p>
      )}
      <ol className="cvm-monitoring-events">
        {query.data?.events.slice(0, 4).map((event, index) => (
          <li key={`${event.at_unix_millis}:${event.kind}:${index}`}>
            <time dateTime={new Date(event.at_unix_millis).toISOString()}>
              {new Date(event.at_unix_millis).toLocaleString('zh-CN')}
            </time>
            <strong>{cvmEventLabel(event.kind)}</strong>
            <span>{event.detail}</span>
          </li>
        ))}
      </ol>
    </Section>
  );
}
