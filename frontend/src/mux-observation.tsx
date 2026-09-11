/* 普通出站 Mux。与反向隧道共用实时帧、状态词和事件形态，但回答的是另一组问题：
 * 业务流是否命中已有 worker、空闲 worker 是否真的消除了按需拨号，以及连接池有没有
 * 因探测、传输或容量拒绝而退化。计数只在浏览器本次打开页面期间做差，不落库。
 *
 * 版面与反向隧道同构：整卡总计在最上，需关注的组保持可见，正常组整体折叠。每组默认
 * 只显示状态、链路、两端、状态分布和 Worker 数量；展开后再按诊断、聚合指标、重点对象、
 * 完整明细的顺序排查，避免多个 picker 同时铺开大量指标与表格。 */
import { useState } from 'react';
import { useNodeRealtime } from './node-realtime';
import {
  filterWorkers,
  MiniStateBar,
  OverviewGroup,
  OverviewStats,
  PriorityWorkers,
  RttLine,
  StateDistribution,
  stateClass,
  wholeSecondAge,
  type WorkerFilter,
} from './observation-overview';
import { PanelTitle } from './ui/icons';

interface PoolConfig {
  concurrency: number;
  prewarm_workers: number;
  reuse_threshold: number;
  max_probing_workers: number;
  probe_interval_ms: number;
  probe_timeout_ms: number;
  idle_ttl_ms: number;
  max_sessions_per_worker: number;
  health_lease_ms: number;
  confirm_timeout_ms: number;
  recovery_successes: number;
  session_end_timeout_ms: number;
}

interface Pool {
  pool_id: string;
  pair: string;
  role: string;
  kind: string;
  used: boolean;
  draining: boolean;
  config: PoolConfig;
  active_sessions: number;
  available_slots: number;
  ready_workers: number;
  total_workers: number;
  dispatches: number;
  active_reuses: number;
  idle_reuses: number;
  demand_dials: number;
  rejected_dispatches: number;
  probes: number;
  acks: number;
  timeouts: number;
  workers_created_demand: number;
  workers_created_warm: number;
  workers_warm_ready: number;
  workers_warm_failed: number;
  workers_closed_idle_ttl: number;
  workers_closed_probe: number;
  workers_closed_capacity: number;
  workers_closed_requests: number;
  workers_closed_transport: number;
  health_suspects: number;
  health_recoveries: number;
  health_draining: number;
  health_queue_failures: number;
  health_dial_throttled: number;
}

interface Worker {
  pool_id: string;
  worker_id: string;
  pair: string;
  role: string;
  kind: string;
  state: string;
  reason: string;
  phase: string;
  active_sessions: number;
  affected_sessions: number;
  available_slots: number;
  lifetime_sessions: number;
  ack_age_ms: number;
  rtt_ms: number;
  probes: number;
  acks: number;
  timeouts: number;
  lease_remaining_ms: number;
  control_queue_depth: number;
  queue_delay_ms: number;
}

interface Transition extends Worker {
  sequence: number;
  at_unix_ms: number;
  from: string;
}

interface Report {
  boot_id: string;
  sequence: number;
  sampled_at_unix_ms: number;
  pools: Pool[];
  workers: Worker[];
  events: Transition[];
}

interface PairId {
  app: string | null;
  chain: string;
  peer: string;
}

interface Counters {
  dispatches: number;
  activeReuses: number;
  idleReuses: number;
  demandDials: number;
  rejected: number;
  warmFailed: number;
  probeTimeouts: number;
  closedProbe: number;
  closedTransport: number;
  suspects: number;
  recoveries: number;
  healthDraining: number;
  queueFailures: number;
  dialThrottled: number;
}

const STALE_MS = 15000;
const SHOWN_EVENTS = 32;

/** 与反向标签相同，普通出站标签也把 app、chain 和对端写在一条稳定标识里。 */
export function parseMuxPair(pair: string): PairId | null {
  const match = /^out:(.+)$/.exec(pair);
  if (!match) return null;
  const at = match[1].lastIndexOf('>');
  if (at <= 0 || at === match[1].length - 1) return null;
  const body = match[1].slice(0, at);
  const slash = body.indexOf('/');
  return {
    app: slash > 0 ? body.slice(0, slash) : null,
    chain: slash > 0 ? body.slice(slash + 1) : body,
    peer: match[1].slice(at + 1),
  };
}

const countersOf = (pool: Pool): Counters => ({
  dispatches: pool.dispatches,
  activeReuses: pool.active_reuses,
  idleReuses: pool.idle_reuses,
  demandDials: pool.demand_dials,
  rejected: pool.rejected_dispatches,
  warmFailed: pool.workers_warm_failed,
  probeTimeouts: pool.timeouts,
  closedProbe: pool.workers_closed_probe,
  closedTransport: pool.workers_closed_transport,
  suspects: pool.health_suspects,
  recoveries: pool.health_recoveries,
  healthDraining: pool.health_draining,
  queueFailures: pool.health_queue_failures,
  dialThrottled: pool.health_dial_throttled,
});

const subtract = (now: Counters, base: Counters): Counters => ({
  dispatches: Math.max(0, now.dispatches - base.dispatches),
  activeReuses: Math.max(0, now.activeReuses - base.activeReuses),
  idleReuses: Math.max(0, now.idleReuses - base.idleReuses),
  demandDials: Math.max(0, now.demandDials - base.demandDials),
  rejected: Math.max(0, now.rejected - base.rejected),
  warmFailed: Math.max(0, now.warmFailed - base.warmFailed),
  probeTimeouts: Math.max(0, now.probeTimeouts - base.probeTimeouts),
  closedProbe: Math.max(0, now.closedProbe - base.closedProbe),
  closedTransport: Math.max(0, now.closedTransport - base.closedTransport),
  suspects: Math.max(0, now.suspects - base.suspects),
  recoveries: Math.max(0, now.recoveries - base.recoveries),
  healthDraining: Math.max(0, now.healthDraining - base.healthDraining),
  queueFailures: Math.max(0, now.queueFailures - base.queueFailures),
  dialThrottled: Math.max(0, now.dialThrottled - base.dialThrottled),
});

/** pool_id 只在一个 Xray 进程内有效，所以 boot_id 一并进入基线键。计数回退也视为新基线。 */
function useObservationBaselines(report: Report | null | undefined) {
  const [state, setState] = useState<{ bootId: string; pools: Record<string, Counters> }>({
    bootId: '',
    pools: {},
  });
  let active = state;
  if (report) {
    const bootChanged = state.bootId !== report.boot_id;
    const needsBaseline = report.pools.some(pool => {
      const old = bootChanged ? undefined : state.pools[pool.pool_id];
      const current = countersOf(pool);
      return !old || current.dispatches < old.dispatches || current.rejected < old.rejected;
    });
    if (bootChanged || needsBaseline) {
      const pools = bootChanged ? {} : { ...state.pools };
      for (const pool of report.pools) {
        const current = countersOf(pool);
        const old = pools[pool.pool_id];
        if (!old || current.dispatches < old.dispatches || current.rejected < old.rejected)
          pools[pool.pool_id] = current;
      }
      active = { bootId: report.boot_id, pools };
      // React applies this adjustment before committing the children, so the first visible frame
      // is already the zero point and no effect-driven second paint is needed.
      setState(active);
    }
  }
  return (bootId: string, pool: Pool) => {
    const current = countersOf(pool);
    const baseline = active.bootId === bootId ? active.pools[pool.pool_id] : undefined;
    return subtract(current, baseline ?? current);
  };
}

const seconds = (ms: number) => `${(ms / 1000).toFixed(2)} s`;
const clock = (unixMs: number) =>
  new Date(unixMs).toLocaleTimeString('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });

/** `neutral` 是「没有 Worker 但也没有失败」：预热为 0 的池在没有业务时就是这个状态，
 *  它不是退化，因此与 ok 一起折进正常那一组。 */
type PoolVerdict = 'ok' | 'warn' | 'neutral' | 'unknown';
const lampOf = (verdict: PoolVerdict) => (verdict === 'ok' ? 'lamp ok' : verdict === 'warn' ? 'lamp warn' : 'lamp');

export function MuxObservationCard({
  nodeId,
  nodeName = id => id,
  chainName = () => null,
}: {
  nodeId: string;
  nodeName?: (id: string) => string;
  chainName?: (app: string | null, chain: string) => string | null;
}) {
  const { last, now, connected } = useNodeRealtime(nodeId);
  const report = last?.event.sample.mux as Report | null | undefined;
  const deltaOf = useObservationBaselines(report);
  const nodeAge = report && last ? last.event.sample.sampled_at_unix_millis - report.sampled_at_unix_ms : Infinity;
  const arrivedAge = last ? now - last.arrived : Infinity;
  const stale =
    !connected ||
    !report ||
    !last ||
    arrivedAge > STALE_MS ||
    now - last.event.received_at_unix_millis > STALE_MS ||
    nodeAge > STALE_MS;

  // 没有启用 worker pool 的出站不会注册观测对象，也不为它占一张空卡。
  if (!report || report.pools.length === 0) return null;

  const workersByPool = new Map<string, Worker[]>();
  for (const worker of report.workers) {
    const workers = workersByPool.get(worker.pool_id) ?? [];
    workers.push(worker);
    workersByPool.set(worker.pool_id, workers);
  }
  const deltas = new Map(report.pools.map(pool => [pool.pool_id, deltaOf(report.boot_id, pool)]));
  // 读数过期时每个池的判定都是 unknown，逐段展开只会把整卡铺开，而铺开的内容并不成立。
  const verdicts = new Map(
    report.pools.map(pool => [
      pool.pool_id,
      verdictOfPool(pool, workersByPool.get(pool.pool_id) ?? [], deltas.get(pool.pool_id)!, stale),
    ]),
  );
  const troubled = stale ? [] : report.pools.filter(pool => verdicts.get(pool.pool_id) === 'warn');
  const rest = stale ? report.pools : report.pools.filter(pool => verdicts.get(pool.pool_id) !== 'warn');
  const total = totalOf(report.pools, deltas);

  const poolSection = (pool: Pool) => (
    <MuxPool
      key={pool.pool_id}
      pool={pool}
      workers={workersByPool.get(pool.pool_id) ?? []}
      delta={deltas.get(pool.pool_id)!}
      verdict={verdicts.get(pool.pool_id)!}
      stale={stale}
      nodeId={nodeId}
      nodeName={nodeName}
      chainName={chainName}
    />
  );

  return (
    <div className="panel ov-card ov-mux-card" aria-label="Mux 连接复用">
      <header>
        <PanelTitle of="outbound">Mux 连接复用</PanelTitle>
        <span className="sp" />
        <span className="hint">每秒采样 · {wholeSecondAge(arrivedAge)} 前 · 实时计算</span>
      </header>

      <p className="rvh-total">
        <span className={lampOf(stale ? 'unknown' : troubled.length > 0 ? 'warn' : 'ok')} />
        <span>
          {report.pools.length} 组 Mux · Worker <b>{stale ? '?' : total.ready}</b> / {total.workers} · 承载业务流{' '}
          <b>{stale ? '?' : total.sessions}</b> · 复用率 <b>{stale ? '?' : total.hitRate}</b>
        </span>
        <span className="rvh-tail">
          {stale ? '数据已过期' : `按需拨号 ${total.demandDials} · 拒绝 ${total.rejected}`}
        </span>
      </p>

      {troubled.length > 0 && (
        <p className="ov-sec bad">
          需关注 {troubled.length} 组 Mux<em>展开查看诊断结论、状态分布与重点对象</em>
        </p>
      )}
      {troubled.map(poolSection)}

      {rest.length > 0 && (
        <details className="rvh-fold rvh-rest">
          <summary>
            {troubled.length > 0 ? '其余 ' : ''}
            {rest.length} 组 Mux · {stale ? '数据已过期' : '运行正常'}
          </summary>
          {rest.map(poolSection)}
        </details>
      )}

      {stale && (
        <p className="rt-dx">
          实时数据连接已中断，或数据已超过 15 秒未更新。当前展示最近一次有效快照，不代表实时状态。
        </p>
      )}

      <details className="rvh-fold">
        <summary>状态事件 · 最近 {Math.min(report.events.length, SHOWN_EVENTS)} 条</summary>
        <EventLog report={report} nodeName={nodeName} />
      </details>
    </div>
  );
}

/** 池的判定。排空也算退化：它不再接收新的业务流，折进「正常」里这件事就读不到了。 */
function verdictOfPool(pool: Pool, workers: Worker[], delta: Counters, stale: boolean): PoolVerdict {
  if (stale) return 'unknown';
  const failures = delta.rejected + delta.warmFailed + delta.probeTimeouts + delta.closedProbe + delta.closedTransport;
  const quarantined = workers.some(worker => worker.state === 'SUSPECT' || worker.state === 'DRAINING');
  if (failures > 0 || quarantined || pool.draining) return 'warn';
  return pool.ready_workers > 0 ? 'ok' : 'neutral';
}

/** 整卡总计。复用率按全部池的派发合计算，不是各池比率的平均——派发量差一个数量级的
 *  两个池取平均会让空闲池的 100% 抵消繁忙池的退化。 */
function totalOf(pools: Pool[], deltas: Map<string, Counters>) {
  let workers = 0;
  let ready = 0;
  let sessions = 0;
  let dispatches = 0;
  let reused = 0;
  let demandDials = 0;
  let rejected = 0;
  for (const pool of pools) {
    workers += pool.total_workers;
    ready += pool.ready_workers;
    sessions += pool.active_sessions;
    const delta = deltas.get(pool.pool_id)!;
    dispatches += delta.dispatches;
    reused += delta.activeReuses + delta.idleReuses;
    demandDials += delta.demandDials;
    rejected += delta.rejected;
  }
  return {
    workers,
    ready,
    sessions,
    demandDials,
    rejected,
    hitRate: dispatches > 0 ? `${Math.round((reused / dispatches) * 100)}%` : '—',
  };
}

/** 每组 Mux 收拢为一行摘要；展开后按排障顺序展示聚合读数与完整明细。 */
function MuxPool({
  pool,
  workers,
  delta,
  verdict,
  stale,
  nodeId,
  nodeName,
  chainName,
}: {
  pool: Pool;
  workers: Worker[];
  delta: Counters;
  verdict: PoolVerdict;
  stale: boolean;
  nodeId: string;
  nodeName: (id: string) => string;
  chainName: (app: string | null, chain: string) => string | null;
}) {
  const id = parseMuxPair(pool.pair);
  const reused = delta.activeReuses + delta.idleReuses;
  const hitRate = delta.dispatches > 0 ? `${Math.round((reused / delta.dispatches) * 100)}%` : '—';
  const quarantined = workers.filter(worker => worker.state === 'SUSPECT' || worker.state === 'DRAINING');
  const failures = delta.rejected + delta.warmFailed + delta.probeTimeouts + delta.closedProbe + delta.closedTransport;
  const hasFinding = failures > 0 || pool.draining || delta.demandDials > 0 || quarantined.length > 0;
  const hasPriority = filterWorkers(workers, '', 'sick').length > 0 || filterWorkers(workers, '', 'slow').length > 0;
  const chain = id ? (chainName(id.app, id.chain) ?? id.chain) : pool.pair;
  const peer = id
    ? `${nodeName(nodeId)} → ${nodeName(id.peer)} · ${pool.kind.toUpperCase()}`
    : `${pool.kind.toUpperCase()} · ${pool.role}`;
  const tone = verdict === 'warn' ? ' v-warn' : '';

  return (
    <details className={`rvh-fold rvh-sub ov-one ov-mux${tone}`}>
      <summary>
        <span className={lampOf(verdict)} />
        <b className={id ? undefined : 'mono'}>{chain}</b>
        <span className="peer">{peer}</span>
        {!stale && <MiniStateBar workers={workers} />}
        <span className="nums">
          Worker <b>{stale ? '?' : pool.ready_workers}</b> / {pool.total_workers}
        </span>
      </summary>
      <div className="ov-onebody">
        {stale ? (
          <>
            <OverviewGroup label="数据状态">
              <OverviewStats>
                <span>数据已过期；以下为最近一次有效快照</span>
              </OverviewStats>
            </OverviewGroup>
            <OverviewGroup label="完整明细">
              <MuxWorkerDetails workers={workers} stale />
            </OverviewGroup>
          </>
        ) : (
          <>
            {hasFinding && (
              <OverviewGroup label="诊断结论">
                <Finding pool={pool} delta={delta} id={id} nodeId={nodeId} nodeName={nodeName} chainName={chainName} />
                {quarantined.length > 0 && (
                  <p className="rt-dx">
                    当前 {quarantined.length} 条 Worker 已暂停接收新业务流；既有业务流继续保留。SUSPECT
                    状态在确认恢复后重新启用，DRAINING 状态在既有业务流结束后回收。
                  </p>
                )}
              </OverviewGroup>
            )}
            <OverviewGroup label="状态分布">
              <StateDistribution workers={workers} empty="当前连接池尚未创建 Worker" />
            </OverviewGroup>
            <OverviewGroup label="时延指标">
              <RttLine workers={workers} noun=" Worker" />
            </OverviewGroup>
            <OverviewGroup label="容量负载">
              <OverviewStats>
                <span>
                  业务流 <b>{pool.active_sessions}</b>
                </span>
                <span>
                  可复用槽位 <b>{pool.available_slots}</b>
                </span>
              </OverviewStats>
            </OverviewGroup>
            <OverviewGroup label="调度统计">
              <OverviewStats>
                <span>
                  派发 <b>{delta.dispatches}</b>
                </span>
                <span>
                  活跃复用 <b>{delta.activeReuses}</b>
                </span>
                <span>
                  空闲命中 <b>{delta.idleReuses}</b>
                </span>
                <span>
                  按需拨号 <b>{delta.demandDials}</b>
                </span>
                <span>
                  拒绝 <b className={delta.rejected > 0 ? 'bad' : undefined}>{delta.rejected}</b>
                </span>
                <span>
                  复用率 <b>{hitRate}</b>
                </span>
              </OverviewStats>
            </OverviewGroup>
            <OverviewGroup label="健康统计">
              <OverviewStats>
                <span>
                  疑似失活 <b>{delta.suspects}</b>
                </span>
                <span>
                  确认恢复 <b>{delta.recoveries}</b>
                </span>
                <span>
                  健康排空 <b>{delta.healthDraining}</b>
                </span>
                <span>
                  控制排队失败 <b className={delta.queueFailures > 0 ? 'bad' : undefined}>{delta.queueFailures}</b>
                </span>
                <span>
                  替换退避拒绝 <b className={delta.dialThrottled > 0 ? 'bad' : undefined}>{delta.dialThrottled}</b>
                </span>
              </OverviewStats>
            </OverviewGroup>
            <OverviewGroup label="运行参数">
              <OverviewStats>
                <span>
                  预热目标 <b>{pool.config.prewarm_workers}</b>
                </span>
                <span>
                  复用阈值 <b>{pool.config.reuse_threshold}</b>
                </span>
                <span>
                  健康租约 <b>{seconds(pool.config.health_lease_ms)}</b>
                </span>
                <span>
                  确认窗口 <b>{seconds(pool.config.confirm_timeout_ms)}</b>
                </span>
                <span>
                  活跃恢复 <b>{pool.config.recovery_successes}</b> 次 ACK
                </span>
                <span>
                  收尾宽限 <b>{seconds(pool.config.session_end_timeout_ms)}</b>
                </span>
              </OverviewStats>
            </OverviewGroup>
            {hasPriority && (
              <OverviewGroup label="重点对象">
                <PriorityWorkers workers={workers} reasonLabel={reasonLabel} />
              </OverviewGroup>
            )}
            <OverviewGroup label="完整明细">
              <MuxWorkerDetails workers={workers} stale={false} />
            </OverviewGroup>
          </>
        )}
        <OverviewGroup label="资源标识">
          <span className="rvh-tag">
            {pool.pair} · {pool.kind}
          </span>
        </OverviewGroup>
      </div>
    </details>
  );
}

function MuxWorkerDetails({ workers, stale }: { workers: Worker[]; stale: boolean }) {
  const [query, setQuery] = useState('');
  const [mode, setMode] = useState<WorkerFilter>('all');
  const filtered = filterWorkers(workers, query, mode);
  return (
    <details className="rvh-fold rvh-sub">
      <summary>
        Worker 明细 · {workers.length} 条{stale ? ' · 数据已过期' : ''}
      </summary>
      <div className="ov-tools">
        <input
          className="f"
          type="search"
          placeholder="筛选 Worker ID"
          aria-label="按 Worker ID 筛选"
          value={query}
          onChange={event => setQuery(event.target.value)}
        />
        <span className="ov-chips">
          <button type="button" aria-pressed={mode === 'all'} onClick={() => setMode('all')}>
            全部状态
          </button>
          <button type="button" aria-pressed={mode === 'sick'} onClick={() => setMode('sick')}>
            非就绪
          </button>
          <button type="button" aria-pressed={mode === 'slow'} onClick={() => setMode('slow')}>
            性能异常
          </button>
        </span>
        <span className="ov-count">{filtered.length} 条结果</span>
      </div>
      <div className="ov-scroll">
        {filtered.length > 0 ? (
          <WorkerTable workers={filtered} stale={stale} />
        ) : (
          <p className="ov-empty">未找到符合筛选条件的条目。</p>
        )}
      </div>
    </details>
  );
}

function WorkerTable({ workers, stale }: { workers: Worker[]; stale: boolean }) {
  if (workers.length === 0) return <p className="note rvh-none">连接池尚未创建 Worker。</p>;
  return (
    <table className="t trvh wide">
      <thead>
        <tr>
          <th>Worker</th>
          <th>状态</th>
          <th className="d2" title="应用探测往返耗时，包含本地排队、链路和对端处理；不是内核 TCP RTT">
            RTT
          </th>
          <th className="d2" title="上一次连接池探测收到匹配 nonce 的应答">
            上次 ACK
          </th>
          <th className="d2" title="只由匹配 Pong 续期；业务有数据不代表双向可用">
            健康租约剩余
          </th>
          <th className="d2" title="待写控制帧数 / 最近一帧开始写入前的排队耗时；不含正在阻塞写入的帧">
            控制排队
          </th>
          <th className="d2">业务流</th>
          <th className="d2">可用槽位</th>
          <th className="d2">累计子连接</th>
          <th className="d2">超时</th>
        </tr>
      </thead>
      <tbody>
        {workers.map(worker => {
          const tone =
            stale || worker.state === 'READY' ? '' : worker.state === 'DEAD' ? 'rvh-row-bad' : 'rvh-row-warn';
          return (
            <tr key={worker.worker_id} className={tone || undefined}>
              <td className="rvh-w">
                {worker.worker_id.slice(-8)}
                <span className="rvh-reason">
                  {worker.phase === 'ending' ? '收尾中' : worker.phase}
                  {worker.reason ? ` · ${reasonLabel(worker.reason)}` : ''}
                </span>
              </td>
              <td>
                <span className={`st ${stale ? 'st-skipped' : stateClass(worker.state)}`}>
                  {stale ? 'UNKNOWN' : worker.state}
                </span>
              </td>
              <td className="d2 mono">
                {stale || worker.rtt_ms <= 0 ? <span className="dim">—</span> : `${worker.rtt_ms} ms`}
              </td>
              <td className="d2 mono">
                {stale ? (
                  <span className="dim">—</span>
                ) : worker.ack_age_ms < 0 ? (
                  '尚未确认'
                ) : (
                  `${seconds(worker.ack_age_ms)} 前`
                )}
              </td>
              <td className="d2 mono">{stale ? '—' : seconds(worker.lease_remaining_ms)}</td>
              <td className="d2 mono">{stale ? '—' : `${worker.control_queue_depth} / ${worker.queue_delay_ms} ms`}</td>
              <td className="d2 mono">{stale ? <span className="dim">—</span> : worker.active_sessions}</td>
              <td className="d2 mono">{stale ? <span className="dim">—</span> : worker.available_slots}</td>
              <td className="d2 mono">{stale ? <span className="dim">—</span> : worker.lifetime_sessions}</td>
              <td className="d2 mono">{stale ? <span className="dim">—</span> : worker.timeouts}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function Finding({
  pool,
  delta,
  id,
  nodeId,
  nodeName,
  chainName,
}: {
  pool: Pool;
  delta: Counters;
  id: PairId | null;
  nodeId: string;
  nodeName: (id: string) => string;
  chainName: (app: string | null, chain: string) => string | null;
}) {
  const who = id
    ? `${chainName(id.app, id.chain) ?? id.chain} ${nodeName(nodeId)} → ${nodeName(id.peer)} ${pool.kind.toUpperCase()}`
    : `${pool.pair} ${pool.kind.toUpperCase()}`;
  const failures = [
    delta.rejected ? `派发拒绝 ${delta.rejected} 次` : '',
    delta.warmFailed ? `预热失败 ${delta.warmFailed} 次` : '',
    delta.probeTimeouts ? `探测超时 ${delta.probeTimeouts} 次` : '',
    delta.closedProbe ? `因探测异常关闭 ${delta.closedProbe} 条 Worker` : '',
    delta.closedTransport ? `因传输异常关闭 ${delta.closedTransport} 条 Worker` : '',
  ].filter(Boolean);
  if (failures.length > 0)
    return (
      <p className="rt-dx">
        <b>{who}</b> 观察窗口内检测到以下异常：{failures.join('、')}。
      </p>
    );
  if (pool.draining)
    return (
      <p className="rt-dx">
        <b>{who}</b> 连接池处于排空状态，已停止接收新业务流。
      </p>
    );
  if (delta.demandDials)
    return (
      <p className="rt-dx">
        <b>{who}</b> 观察窗口内发生 {delta.demandDials} 次按需拨号。无健康空闲 Worker 时，未达到复用阈值则创建新
        Worker；达到阈值后，仅在可用槽位耗尽时扩容。
      </p>
    );
  return null;
}

function reasonLabel(reason: string): string {
  switch (reason) {
    case 'capacity_reclaim':
      return '基础容量回收 · capacity_reclaim';
    case 'session_ending':
      return '等待收尾写入 · session_ending';
    case 'session_end_timeout':
      return '收尾写入超时 · session_end_timeout';
    case 'health_lease_expired':
      return '健康租约到期 · health_lease_expired';
    case 'probe_timeout':
      return '探测超时 · probe_timeout';
    case 'confirmation_timeout':
      return '确认超时，保留旧流排空 · confirmation_timeout';
    case 'confirmed_recovery':
      return '连续 ACK 确认恢复 · confirmed_recovery';
    case 'idle_revalidated':
      return '旧流已结束，空闲重新验证通过 · idle_revalidated';
    case 'control_queue_full_or_closed':
      return '控制队列不可用 · control_queue_full_or_closed';
    default:
      return reason;
  }
}

function EventLog({ report, nodeName }: { report: Report; nodeName: (id: string) => string }) {
  const shown = report.events.slice(-SHOWN_EVENTS).reverse();
  if (shown.length === 0) return <p className="note rvh-none">没有事件。</p>;
  const label = (event: Transition) => {
    const id = parseMuxPair(event.pair);
    const pair = id ? `${id.chain} · ${nodeName(id.peer)}` : event.pair;
    return `${pair} · ${event.kind.toUpperCase()}`;
  };
  return (
    <div className="rvh-log">
      {shown.map((event, index) => {
        const older = shown[index + 1];
        const gap = older && event.sequence - older.sequence > 1;
        const tone =
          event.state === 'DEAD'
            ? 'bad'
            : event.state === 'SUSPECT' || event.state === 'DRAINING' || event.state === 'CLOSED'
              ? 'warn'
              : event.state === 'READY'
                ? 'ok'
                : '';
        return (
          <div key={`${report.boot_id}:${event.sequence}`}>
            <div className={`rvh-ev ${tone}`}>
              <time>
                {clock(event.at_unix_ms)}
                <u>#{event.sequence}</u>
              </time>
              <div>
                <b>
                  {label(event)} · {event.worker_id.slice(-8)}
                </b>{' '}
                <span className="rvh-arrow">
                  {event.from} → {event.state}
                </span>
                <i>
                  {reasonLabel(event.reason)}
                  {event.affected_sessions ? ` · 受影响业务流 ${event.affected_sessions}` : ''}
                </i>
              </div>
            </div>
            {gap && (
              <p className="rvh-gap">
                缺 {event.sequence - older.sequence - 1} 条 · #{older.sequence + 1}–#{event.sequence - 1}{' '}
                没有出现在任何一份快照里
              </p>
            )}
          </div>
        );
      })}
      {report.events.length > SHOWN_EVENTS && (
        <p className="rvh-gap">
          更早的 {report.events.length - SHOWN_EVENTS} 条在这份快照里但未显示 · 上游只保留最近 256 条
        </p>
      )}
    </div>
  );
}
