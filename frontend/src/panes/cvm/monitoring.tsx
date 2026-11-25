import { lazy, Suspense, useState } from 'react';
import { fetchCvmMonitoring, type CvmInstance, type CvmMonitoring } from '../../api';
import { navigate } from '../../forge/route';
import { Ago, ErrorBox, SegmentedControl } from '../../ui/bits';
import { useNow } from '../../ui/clock';
import { PanelLoading } from '../../ui/loading';
import { useMetricQuery } from '../../ui/metric-cadence';
import { cvmEventLabel } from './monitoring-data';
import { PanelTitle } from '../../ui/icons';
import { RuntimeBand, RuntimeCells } from '../../ui/runtime-band';

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

// Observations remain useful in a redacted workspace, but arbitrary error and event details can contain addresses or IDs.
function redactDiagnosticText(data: CvmMonitoring): CvmMonitoring {
  return {
    ...data,
    error: data.error ? '实例监控采集失败。' : null,
    events: data.events.map(event => ({ ...event, detail: '' })),
    health: data.health ? { ...data.health, runtime_id: null, error: null } : null,
  };
}

export function useCvmObservation(id: string, seconds = 3600, redacted = false) {
  const result = useMetricQuery({
    queryKey: ['cvm-monitoring', id, seconds],
    queryFn: () => fetchCvmMonitoring(id, seconds),
    staleTime: 15_000,
    refetchInterval: 15_000,
    select: redacted ? redactDiagnosticText : undefined,
    metricFacts: data => [redacted, data.state, data.error, ...data.issues.map(i => i.message)],
  });
  const query = redacted && result.error ? { ...result, error: new Error('无法读取实例监控。') } : result;
  const now = useNow();
  const data = query.data;
  const latest = data?.latest;
  const health = data?.health;
  const stopped = health?.runtime_state === 'stopped';
  const serverNow = data ? data.range_end_unix_millis + Math.max(0, now - query.dataUpdatedAt) : now;
  const stale = !!latest && serverNow - latest.observed_at_unix_millis > 120_000;
  const healthStale = !!health && serverNow - health.observed_at_unix_millis > 120_000;
  const current = !!latest && !query.error && !stopped && !stale && data?.state === 'live';
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
  return { query, latest, health, healthStale, stopped, current, label };
}

export function CvmMonitoringPanel({
  instance,
  redacted = false,
  showHostLink = true,
}: {
  instance: CvmInstance;
  redacted?: boolean;
  showHostLink?: boolean;
}) {
  const [seconds, setSeconds] = useState(3600);
  const [nic, setNic] = useState('');
  const { query, latest, health, healthStale, stopped, current, label } = useCvmObservation(
    instance.desired.id,
    seconds,
    redacted,
  );
  const data = query.data;
  const nics = latest?.interfaces ?? [];
  const selected = nics.some(item => item.id === nic) ? nic : '';
  const healthFacts = [
    [
      '运行时',
      healthLabel(health?.runtime_state),
      instance.desired.spec.execution.kind === 'baremetal' ? '宿主操作系统状态' : 'Cube 运行状态',
    ],
    [
      instance.desired.spec.execution.kind === 'baremetal' ? '本机采样' : 'Guest 采样通道',
      healthLabel(health?.guest_state),
      instance.desired.spec.execution.kind === 'baremetal' ? '宿主本机读取' : 'Guest 管理连接',
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
  const native = instance.desired.spec.execution.kind === 'baremetal';
  const issues = !!data?.issues?.length && (
    <ul className="cvm-monitoring-issues" aria-label="需要关注">
      {data.issues.map((issue, i) => (
        <li key={`${issue.kind}:${i}`} className={issue.severity === 'bad' ? 'error' : 'warn'}>
          {issue.message}
        </li>
      ))}
    </ul>
  );
  const range = seconds === 3600 ? '近 1 小时' : seconds === 21600 ? '近 6 小时' : '近 24 小时';
  return (
    <div className="nd-tab-observed cvm-monitoring-view" role="region" aria-label="容器 监控">
      <div className="cvm-monitor-bar">
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
        {showHostLink && (
          <button className="btn" onClick={() => navigate('nodes', { p: 'node', id: instance.node_id })}>
            宿主观测
          </button>
        )}
        <span className="cvm-monitor-sampling" title="独立采样，不受配置任务是否成功影响；缺失与重启区间断开">
          <i className={`cg-lamp ${current ? 'ok' : stopped ? 'idle' : 'warn'}`} aria-hidden="true" />
          {label}
          {latest && (
            <>
              {' · '}
              <Ago at={new Date(latest.observed_at_unix_millis).toISOString()} />
            </>
          )}
        </span>
      </div>
      {query.isPending && <PanelLoading label="读取实例监控" />}
      {query.error && <ErrorBox error={query.error} />}
      {data?.error && <p className="cvm-notice">{data.error}</p>}
      {!query.isPending && !latest && !query.error && (
        <p className="note">
          {stopped ? '实例已关机，开机后继续采样。' : '尚无实例监控数据。宿主更新采集能力后会自动上报。'}
        </p>
      )}
      {issues}
      {data?.samples_truncated && <p className="note">所选区间超过展示上限，只显示最近的历史记录。</p>}
      {latest && data && (
        <Suspense fallback={<PanelLoading label="加载监控曲线" />}>
          <Charts data={data} nic={selected} latest={latest} />
        </Suspense>
      )}
      <div className="panel nd-runtime cvm-runtime">
        <header>
          <PanelTitle of="diag">诊断探测</PanelTitle>
          <span className="hint">
            {health ? (
              <>
                健康观测于 <Ago at={new Date(health.observed_at_unix_millis).toISOString()} />
              </>
            ) : (
              '尚无健康观测'
            )}
          </span>
        </header>
        <RuntimeBand name="PROBE" icon="diag" meta={healthStale ? '读数已过期' : label}>
          <RuntimeCells
            items={healthFacts.map(([name, value, scope]) => {
              const tone = healthStale ? 'warn' : healthTone(value);
              return [
                name,
                <span title={scope} className={`cvm-probe cvm-probe-${tone}`}>
                  {healthStale ? '读数已过期' : value}
                  <span className="nd-rt-q">{scope}</span>
                </span>,
                'w2',
              ];
            })}
          />
        </RuntimeBand>
        <p className="note cvm-probe-note">
          本地端口探测不代表公网链路可达；UDP
          服务需单独拨测。历史按分钟聚合，保留采样峰值和最低可用内存，缺失与重启区间断开，未知值显示为 —。
        </p>
      </div>
      <section className="panel titled cvm-net-panel" aria-label="事件与操作">
        <header>
          <PanelTitle of="clock">事件与操作</PanelTitle>
          <span className="hint">{range}</span>
        </header>
        {!data?.events?.length ? (
          <p className="note cvm-net-empty">所选时间内没有事件记录。</p>
        ) : (
          <div className="cvm-monitoring-event-list">
            <EventList events={data.events} native={native} full />
          </div>
        )}
      </section>
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

export function EventList({
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
            {/* 概览只列近 1 小时，时刻足以区分；监控页的区间可达 24 小时，写完整日期。 */}
            {new Date(event.at_unix_millis).toLocaleString('zh-CN', full ? undefined : { timeStyle: 'short' })}
          </time>
          {event.detail && <span>{event.detail}</span>}
        </li>
      ))}
    </ol>
  );
}
