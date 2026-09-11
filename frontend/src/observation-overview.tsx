import type { ReactNode } from 'react';

/** 两张实时观测卡共用的 Worker 最小字段集。 */
export interface OverviewWorker {
  worker_id: string;
  state: string;
  reason: string;
  active_sessions: number;
  rtt_ms: number;
  timeouts: number;
  queue_delay_ms: number;
}

interface Segment {
  state: string;
  label: string;
  className: string;
  color: string;
  count: number;
}

const STATE_ORDER = ['READY', 'CONNECTING', 'VALIDATING', 'DRAINING', 'SUSPECT', 'DEAD', 'CLOSED'];
const STATE_META: Record<string, Omit<Segment, 'state' | 'count'>> = {
  READY: { label: '就绪', className: 's-ok', color: 'var(--ok)' },
  CONNECTING: { label: '连接中', className: 's-run', color: 'var(--action)' },
  VALIDATING: { label: '验证中', className: 's-run', color: 'var(--action)' },
  DRAINING: { label: '排空中', className: 's-idle', color: 'var(--ink-4)' },
  SUSPECT: { label: '待确认', className: 's-warn', color: 'var(--gold)' },
  DEAD: { label: '已关闭', className: 's-bad', color: 'var(--err)' },
  CLOSED: { label: '已关闭', className: 's-idle', color: 'var(--ink-4)' },
};

export type WorkerFilter = 'all' | 'sick' | 'slow';

/** 新鲜度只显示整秒；刚收到的帧稳定显示为 0 s，不再出现毫秒或小数秒。 */
export function wholeSecondAge(ms: number): string {
  return `${Number.isFinite(ms) ? Math.floor(Math.max(0, ms) / 1000) : '?'} s`;
}

export function stateClass(state: string): string {
  switch (state) {
    case 'READY':
      return 'st-ok';
    case 'CONNECTING':
    case 'VALIDATING':
      return 'st-running';
    case 'SUSPECT':
      return 'st-warn';
    case 'DEAD':
      return 'st-halted';
    default:
      return 'st-skipped';
  }
}

function segmentsOf(workers: readonly OverviewWorker[]): Segment[] {
  const counts = new Map<string, number>();
  for (const worker of workers) counts.set(worker.state, (counts.get(worker.state) ?? 0) + 1);
  const states = [
    ...STATE_ORDER.filter(state => counts.has(state)),
    ...[...counts.keys()].filter(state => !STATE_ORDER.includes(state)).sort(),
  ];
  return states.map(state => ({
    state,
    count: counts.get(state)!,
    ...(STATE_META[state] ?? { label: state, className: 's-idle', color: 'var(--ink-4)' }),
  }));
}

function DistributionBar({ segments }: { segments: Segment[] }) {
  return (
    <span className="ov-bar" role="img" aria-label={segments.map(item => `${item.label} ${item.count}`).join('，')}>
      {segments.map(item => (
        <i key={item.state} className={item.className} style={{ flex: item.count }} />
      ))}
    </span>
  );
}

export function MiniStateBar({ workers }: { workers: readonly OverviewWorker[] }) {
  const segments = segmentsOf(workers);
  return segments.length > 0 ? <DistributionBar segments={segments} /> : null;
}

export function StateDistribution({ workers, empty }: { workers: readonly OverviewWorker[]; empty: string }) {
  const segments = segmentsOf(workers);
  if (segments.length === 0)
    return (
      <p className="ov-stats">
        <span>{empty}</span>
      </p>
    );
  return (
    <div className="ov-mix">
      <DistributionBar segments={segments} />
      <p className="ov-legend">
        {segments.map(item => (
          <span key={item.state}>
            <i style={{ background: item.color }} />
            {item.label} <b>{item.count}</b>
          </span>
        ))}
      </p>
    </div>
  );
}

export interface RttSummary {
  p50: number;
  p95: number;
  max: number;
}

export function rttSummary(workers: readonly OverviewWorker[]): RttSummary {
  const values = workers
    .filter(worker => worker.state === 'READY' && worker.rtt_ms > 0)
    .map(worker => worker.rtt_ms)
    .sort((a, b) => a - b);
  const quantile = (point: number) =>
    values.length > 0 ? values[Math.min(values.length - 1, Math.floor(values.length * point))] : 0;
  return { p50: quantile(0.5), p95: quantile(0.95), max: values.at(-1) ?? 0 };
}

export function RttLine({ workers, noun }: { workers: readonly OverviewWorker[]; noun: string }) {
  const rtt = rttSummary(workers);
  if (rtt.max === 0)
    return (
      <OverviewStats>
        <span>
          <b>—</b> 当前无就绪{noun}，无法计算往返时延
        </span>
      </OverviewStats>
    );
  return (
    <OverviewStats>
      <span>
        p50 <b>{rtt.p50}</b> ms
      </span>
      <span>
        p95 <b>{rtt.p95}</b> ms
      </span>
      <span>
        最大 <b className={rtt.max > rtt.p95 * 1.6 ? 'warn' : undefined}>{rtt.max}</b> ms
      </span>
    </OverviewStats>
  );
}

export function isPerformanceOutlier(worker: OverviewWorker, p95: number): boolean {
  return (
    worker.state === 'READY' &&
    (worker.rtt_ms > Math.max(p95, 1) * 1.5 || worker.timeouts > 0 || worker.queue_delay_ms > 60)
  );
}

export function filterWorkers<T extends OverviewWorker>(workers: readonly T[], query: string, mode: WorkerFilter): T[] {
  const normalized = query.trim().toLowerCase();
  const { p95 } = rttSummary(workers);
  return workers.filter(
    worker =>
      (!normalized || worker.worker_id.toLowerCase().includes(normalized)) &&
      (mode === 'all' ||
        (mode === 'sick' && worker.state !== 'READY') ||
        (mode === 'slow' && isPerformanceOutlier(worker, p95))),
  );
}

export function PriorityWorkers({
  workers,
  reasonLabel = reason => reason,
}: {
  workers: readonly OverviewWorker[];
  reasonLabel?: (reason: string) => string;
}) {
  const { p95 } = rttSummary(workers);
  const nonReady = workers.filter(worker => worker.state !== 'READY');
  const outliers = workers.filter(worker => isPerformanceOutlier(worker, p95));
  const shown = [...nonReady.slice(0, 3), ...outliers.slice(0, Math.max(0, 3 - nonReady.length))];
  if (shown.length === 0) return null;

  const reasonOf = (worker: OverviewWorker) => {
    if (worker.state !== 'READY') return reasonLabel(worker.reason) || '—';
    const reasons: string[] = [];
    if (worker.rtt_ms > Math.max(p95, 1) * 1.5)
      reasons.push(`RTT ${worker.rtt_ms} ms，为 p95 的 ${(worker.rtt_ms / Math.max(p95, 1)).toFixed(1)} 倍`);
    if (worker.timeouts > 0) reasons.push(`探测超时 ${worker.timeouts} 次`);
    if (worker.queue_delay_ms > 60) reasons.push(`控制帧排队时延 ${worker.queue_delay_ms} ms`);
    return reasons.join(' · ');
  };
  const toneOf = (worker: OverviewWorker) =>
    worker.state === 'DEAD'
      ? ' v-bad'
      : worker.state === 'SUSPECT'
        ? ' v-warn'
        : worker.state === 'READY'
          ? ''
          : ' v-idle';
  const more = nonReady.length + outliers.length - shown.length;

  return (
    <div className="ov-picks">
      {shown.map(worker => (
        <div key={worker.worker_id} className={`ov-pick${toneOf(worker)}`}>
          <span className="id">{worker.worker_id.slice(-8)}</span>
          <span className={`st ${stateClass(worker.state)}`}>{worker.state}</span>
          <span className="why">{reasonOf(worker)}</span>
          <span className="nums">
            业务流 {worker.active_sessions} · 探测超时 {worker.timeouts}
          </span>
        </div>
      ))}
      {more > 0 && (
        <p className="ov-note">另有 {more} 条符合条件的对象，可在「完整明细」中按「非就绪」或「性能异常」筛选。</p>
      )}
    </div>
  );
}

export function OverviewGroup({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="ov-g">
      <span className="ov-gl">{label}</span>
      <div className="ov-gv">{children}</div>
    </div>
  );
}

export function OverviewStats({ children }: { children: ReactNode }) {
  return <p className="ov-stats">{children}</p>;
}
