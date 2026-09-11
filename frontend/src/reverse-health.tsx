/* 反向隧道。机器详情「观测」页的一张卡。
 *
 * ## 这张卡要回答的两个问题，顺序不能反
 *
 * 「现在能不能用」和「为什么不能用」都由隧道读数回答。目标应用可用性由独立的
 * 端到端拨测负责，不在反向隧道内重复发起 HTTP 请求。
 *
 * 版面按「异常优先」收敛：整卡总计在最上，需关注的组保持可见，正常组整体折叠。每组默认
 * 只显示状态、链路、两端、状态分布和就绪数量；展开后再按诊断、聚合指标、重点对象、完整
 * 明细的顺序排查。一台机器可以有几百条隧道，完整表固定在可筛选的滚动区内。
 *
 * ## 数据是瞬时的
 *
 * agent 每秒读一次 xray 的快照，挂在实时采样帧上；控制台的环只保留最新一帧的
 * reverse_health（`console/src/realtime.rs:258`），不落库。因此这里没有趋势线，
 * 断流或超过 15 秒未更新时整张卡的读数都不成立——该判定写在卡头，不散在每个值上。
 *
 * 事件同理：上游只保留最近 256 条尾部，xray 重启即清零。序号不连续时在时间线里标出
 * 缺口，否则一段读不到的区间会被读成「这期间没有发生任何事」。
 */
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

interface Worker {
  active_sessions: number;
  affected_sessions: number;
  control_queue_depth: number;
  queue_delay_ms: number;
  scheduler_lag_ms: number;
  worker_id: string;
  pair: string;
  role: string;
  state: string;
  reason: string;
  ack_age_ms: number;
  rtt_ms: number;
  probes: number;
  acks: number;
  timeouts: number;
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
  workers: Worker[];
  events: Transition[];
}
/** 读数过期的门限。节点时间只与节点时间比较，浏览器侧的新鲜度用收到的时刻算。 */
const STALE_MS = 15000;
/** 时间线一次显示的条数。上游保留 256 条，全部铺开会把这张卡变成日志页。 */
const SHOWN_EVENTS = 32;

/* ── 标识 ─────────────────────────────────────────────────────── */

type Role = 'portal' | 'bridge';
interface PairId {
  role: Role;
  app: string | null;
  chain: string;
  peer: string;
}

/** 上报的 `pair` 是控制台下发给 xray 的反向标签（`physical/node.rs:1936`）：
 *  `rev:portal:{app}/{chain}>{peer}`、`rev:bridge:{app}/{chain}<{peer}`，app 段可缺省。
 *  链在标签里——同一对机器上的两条链是两个标签，只显示机器名会出现两行同名的对。
 *  这个串由上游生成，格式对不上时返回 null 并原样显示，不猜。 */
export function parsePair(pair: string): PairId | null {
  const head = /^rev:(portal|bridge):(.+)$/.exec(pair);
  if (!head) return null;
  const role = head[1] as Role;
  // portal 端的标签写下一跳，bridge 端写上一跳，两者的分隔符不同（见 node.rs 的注释）。
  const at = head[2].lastIndexOf(role === 'portal' ? '>' : '<');
  if (at <= 0 || at === head[2].length - 1) return null;
  const body = head[2].slice(0, at);
  const slash = body.indexOf('/');
  return {
    role,
    app: slash > 0 ? body.slice(0, slash) : null,
    chain: slash > 0 ? body.slice(slash + 1) : body,
    peer: head[2].slice(at + 1),
  };
}

/** 这一对的两端，按链序。portal 端本机是起点，bridge 端本机是终点。 */
function ends(id: PairId, nodeId: string): [string, string] {
  return id.role === 'portal' ? [nodeId, id.peer] : [id.peer, nodeId];
}

/* ── 判定 ─────────────────────────────────────────────────────── */

type Verdict = 'ok' | 'warn' | 'bad' | 'unknown';

interface Pair {
  pair: string;
  id: PairId | null;
  role: string;
  workers: Worker[];
}

const readyOf = (p: Pair) => p.workers.filter(w => w.state === 'READY').length;
const sessionsOf = (workers: Worker[]) => workers.reduce((n, w) => n + w.active_sessions, 0);

/** 一对的总判定。这里不判「可用数量低于下限」——下限（`min_healthy_workers`）是可按链路覆盖的策略，
 *  在界面上复算一遍会与编译器的解析结果分叉；隔离和关闭的隧道本身已经是可观测的事实。 */
function verdictOf(p: Pair, stale: boolean): Verdict {
  if (stale) return 'unknown';
  if (p.workers.length > 0 && readyOf(p) === 0) return 'bad';
  if (p.workers.some(w => w.state === 'SUSPECT' || w.state === 'DEAD')) return 'warn';
  return 'ok';
}

const lampOf = (v: Verdict) =>
  v === 'ok' ? 'lamp ok' : v === 'bad' ? 'lamp bad' : v === 'warn' ? 'lamp warn' : 'lamp';

/* ── 格式 ─────────────────────────────────────────────────────── */

const seconds = (ms: number) => `${(ms / 1000).toFixed(2)} s`;
/* 24 小时制，与观测页其他时间轴一致（telemetry.tsx 同一组参数）。12 小时制多出的
   AM/PM 会把时间线那一列挤成两行。 */
const clock = (unixMs: number) =>
  new Date(unixMs).toLocaleTimeString('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });

/* ── 卡片 ─────────────────────────────────────────────────────── */

export function ReverseHealthCard({
  nodeId,
  nodeName = id => id,
  chainName = () => null,
}: {
  nodeId: string;
  /** 机器名。与 LINK QUALITY 一样由调用方传入，这张卡不自己拉一份机器列表。 */
  nodeName?: (id: string) => string;
  /** 链名。标签里只有 id，名称来自快照；取不到时只显示 id。 */
  chainName?: (app: string | null, chain: string) => string | null;
}) {
  const { last, now, connected } = useNodeRealtime(nodeId);
  const report = last?.event.sample.reverse_health as Report | null | undefined;
  // Compare node timestamps only with node timestamps; browser freshness uses receipt time.
  const nodeAge = report && last ? last.event.sample.sampled_at_unix_millis - report.sampled_at_unix_ms : Infinity;
  const arrivedAge = last ? now - last.arrived : Infinity;
  const stale =
    !connected ||
    !report ||
    !last ||
    arrivedAge > STALE_MS ||
    now - last.event.received_at_unix_millis > STALE_MS ||
    nodeAge > STALE_MS;

  // 没有反向隧道的机器不显示这张卡。整个机队里绝大多数机器不做反向，常驻一张
  // 「没有反向隧道」的空卡会把观测页每一页都撑长一节，而它不携带任何读数。
  // 曾经上报过、随后断流的机器仍然显示——那时的空白才是需要解释的。
  if (!report || report.workers.length === 0) return null;

  const pairs = groupPairs(report);
  // 分组依据是「这一对是不是全部就绪」，不是 verdictOf——DRAINING 不进判定（它在排空，
  // 不是故障），但那一对的判定带会写「1 条未就绪」，折进「全部就绪」里两句话就对不上。
  // 读数过期时每一对的判定都是 unknown，逐段展开只会把整卡铺开，而铺开的内容并不成立；
  // 因此过期时全部按「不展开」处理，卡头和卡尾的过期说明负责解释这段空白。
  const settled = (p: Pair) => readyOf(p) === p.workers.length;
  const troubled = stale ? [] : pairs.filter(p => !settled(p));
  const rest = stale ? pairs : pairs.filter(settled);
  const total = totalOf(pairs);
  const verdict: Verdict = stale
    ? 'unknown'
    : pairs.some(p => verdictOf(p, false) === 'bad')
      ? 'bad'
      : troubled.length > 0
        ? 'warn'
        : 'ok';

  return (
    <div className="panel ov-card ov-rev-card" aria-label="反向隧道">
      <header>
        <PanelTitle of="tunnels">反向隧道</PanelTitle>
        <span className="sp" />
        <span className="hint">每秒采样 · {wholeSecondAge(arrivedAge)} 前 · 实时计算</span>
      </header>

      <p className="rvh-total">
        <span className={lampOf(verdict)} />
        <span>
          {pairs.length} 组反向 · 隧道 <b>{stale ? '?' : total.ready}</b> / {total.workers} 就绪 · 承载业务流{' '}
          <b>{stale ? '?' : total.sessions}</b>
        </span>
        <span className="rvh-tail">
          {stale ? '数据已过期' : troubled.length > 0 ? `${troubled.length} 组反向未就绪` : '全部就绪'}
        </span>
      </p>

      {troubled.length > 0 && (
        <p className="ov-sec bad">
          需关注 {troubled.length} 组反向<em>展开查看诊断结论、状态分布与重点对象</em>
        </p>
      )}
      {troubled.map(p => (
        <PairRow key={`p:${p.pair}`} pair={p} stale={stale} nodeId={nodeId} nodeName={nodeName} chainName={chainName} />
      ))}

      {rest.length > 0 && (
        <details className="rvh-fold rvh-rest">
          <summary>
            {troubled.length > 0 ? '其余 ' : ''}
            {rest.length} 组反向 · {stale ? '数据已过期' : '全部就绪'}
          </summary>
          {rest.map(p => (
            <PairRow
              key={`p:${p.pair}`}
              pair={p}
              stale={stale}
              nodeId={nodeId}
              nodeName={nodeName}
              chainName={chainName}
            />
          ))}
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

/** worker 按 `pair` 归组。 */
function groupPairs(report: Report): Pair[] {
  const byPair = new Map<string, Pair>();
  const take = (pair: string, role: string) => {
    const found = byPair.get(pair);
    if (found) return found;
    const id = parsePair(pair);
    const made: Pair = { pair, id, role: id?.role ?? role, workers: [] };
    byPair.set(pair, made);
    return made;
  };
  for (const w of report.workers) take(w.pair, w.role).workers.push(w);
  return [...byPair.values()];
}

/** 整卡总计。几百对时它是唯一一眼能读完的读数，逐对的内容都在它下面。 */
function totalOf(pairs: Pair[]) {
  let workers = 0;
  let ready = 0;
  let sessions = 0;
  for (const p of pairs) {
    workers += p.workers.length;
    ready += readyOf(p);
    sessions += sessionsOf(p.workers);
  }
  return { workers, ready, sessions };
}

/** 每组反向连接收拢为一行摘要；展开后按排障顺序展示聚合读数与完整明细。 */
function PairRow({
  pair,
  stale,
  nodeId,
  nodeName,
  chainName,
}: {
  pair: Pair;
  stale: boolean;
  nodeId: string;
  nodeName: (id: string) => string;
  chainName: (app: string | null, chain: string) => string | null;
}) {
  const verdict = verdictOf(pair, stale);
  const ready = readyOf(pair);
  const settled = ready === pair.workers.length;
  const [from, to] = pair.id ? ends(pair.id, nodeId) : [null, null];
  const chain = pair.id ? (chainName(pair.id.app, pair.id.chain) ?? pair.id.chain) : pair.pair;
  const peer = pair.id ? `${nodeName(from!)} → ${nodeName(to!)}` : pair.role;
  const tail = stale ? '数据已过期' : settled ? '全部就绪' : `${pair.workers.length - ready} 条未就绪`;
  const tone = verdict === 'bad' ? ' v-bad' : verdict === 'warn' ? ' v-warn' : '';
  const hasPriority =
    filterWorkers(pair.workers, '', 'sick').length > 0 || filterWorkers(pair.workers, '', 'slow').length > 0;

  return (
    <details className={`rvh-fold rvh-sub ov-one ov-rev${tone}`}>
      <summary>
        <span className={lampOf(verdict)} />
        <b className={pair.id ? undefined : 'mono'}>{chain}</b>
        <span className="peer">{peer}</span>
        {!stale && <MiniStateBar workers={pair.workers} />}
        <span className="nums">
          隧道 <b>{stale ? '?' : ready}</b> / {pair.workers.length} · <span className="tail">{tail}</span>
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
              <ReverseWorkerDetails workers={pair.workers} stale />
            </OverviewGroup>
          </>
        ) : (
          <>
            {verdict !== 'ok' && (
              <OverviewGroup label="诊断结论">
                <Findings pair={pair} nodeId={nodeId} nodeName={nodeName} chainName={chainName} />
              </OverviewGroup>
            )}
            <OverviewGroup label="状态分布">
              <StateDistribution workers={pair.workers} empty="当前隧道对未包含任何隧道" />
            </OverviewGroup>
            <OverviewGroup label="时延指标">
              <RttLine workers={pair.workers} noun="隧道" />
            </OverviewGroup>
            <OverviewGroup label="容量负载">
              <OverviewStats>
                <span>
                  业务流 <b>{sessionsOf(pair.workers)}</b>
                </span>
                <span>
                  就绪隧道 <b>{ready}</b> / {pair.workers.length}
                </span>
                <span>
                  角色 <b>{pair.role}</b>
                </span>
              </OverviewStats>
            </OverviewGroup>
            {hasPriority && (
              <OverviewGroup label="重点对象">
                <PriorityWorkers workers={pair.workers} />
              </OverviewGroup>
            )}
            <OverviewGroup label="完整明细">
              <ReverseWorkerDetails workers={pair.workers} stale={false} />
            </OverviewGroup>
          </>
        )}
        <OverviewGroup label="资源标识">
          <span className="rvh-tag">{pair.pair}</span>
        </OverviewGroup>
      </div>
    </details>
  );
}

function ReverseWorkerDetails({ workers, stale }: { workers: Worker[]; stale: boolean }) {
  const [query, setQuery] = useState('');
  const [mode, setMode] = useState<WorkerFilter>('all');
  const filtered = filterWorkers(workers, query, mode);
  return (
    <details className="rvh-fold rvh-sub">
      <summary>
        隧道明细 · {workers.length} 条{stale ? ' · 数据已过期' : ''}
      </summary>
      <div className="ov-tools">
        <input
          className="f"
          type="search"
          placeholder="筛选隧道 ID"
          aria-label="按隧道 ID 筛选"
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

/** 逐条隧道。异常行整行浅底——一行里最先被读到的应当是「哪一条出了问题」，
 *  而不是某一列的数值。 */
function WorkerTable({ workers, stale }: { workers: Worker[]; stale: boolean }) {
  if (workers.length === 0) return <p className="note rvh-none">这一对当前没有隧道。</p>;
  return (
    <table className="t trvh">
      <thead>
        <tr>
          <th>隧道</th>
          <th>状态</th>
          <th className="d2">RTT</th>
          <th className="d2" title="上一次收到匹配 nonce 的应答">
            上次 ACK
          </th>
          <th className="d2" title="这条隧道上正在承载的业务连接数">
            业务流
          </th>
          <th className="d2" title="控制帧排队时间 / 调度延迟。两项都计入探测预算">
            排队 / 调度
          </th>
          <th className="d2">超时</th>
        </tr>
      </thead>
      <tbody>
        {workers.map(w => {
          const tone = stale ? '' : w.state === 'DEAD' ? 'rvh-row-bad' : w.state === 'SUSPECT' ? 'rvh-row-warn' : '';
          const queue = w.queue_delay_ms + w.scheduler_lag_ms;
          return (
            <tr key={w.worker_id} className={tone || undefined}>
              <td className="rvh-w">
                {w.worker_id.slice(-8)}
                {w.reason && !stale && <span className="rvh-reason">{w.reason}</span>}
              </td>
              <td>
                <span className={`st ${stale ? 'st-skipped' : stateClass(w.state)}`}>
                  {stale ? 'UNKNOWN' : w.state}
                </span>
              </td>
              <td className="d2 mono">{stale || w.rtt_ms <= 0 ? <span className="dim">—</span> : `${w.rtt_ms} ms`}</td>
              <td className="d2 mono">
                {stale ? <span className="dim">—</span> : w.ack_age_ms < 0 ? '尚未确认' : `${seconds(w.ack_age_ms)} 前`}
              </td>
              <td className="d2 mono">
                {stale ? <span className="dim">—</span> : w.active_sessions || <span className="dim">0</span>}
              </td>
              <td className="d2 mono">
                {stale || queue === 0 ? (
                  <span className="dim">—</span>
                ) : (
                  `${w.queue_delay_ms} ms / ${w.scheduler_lag_ms} ms`
                )}
              </td>
              <td className="d2 mono">
                {stale ? <span className="dim">—</span> : w.timeouts || <span className="dim">0</span>}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/** 判定句。语气与 LINK QUALITY 的 .rt-dx 一致：正常的对不出现在这里。 */
function Findings({
  pair,
  nodeId,
  nodeName,
  chainName,
}: {
  pair: Pair;
  nodeId: string;
  nodeName: (id: string) => string;
  chainName: (app: string | null, chain: string) => string | null;
}) {
  const verdict = verdictOf(pair, false);
  if (verdict === 'ok') return null;
  // 判定句里的名字与判定带同源：那里写链名，这里写 chain id 会读成两条不同的链。
  const who = pair.id
    ? `${chainName(pair.id.app, pair.id.chain) ?? pair.id.chain} ${ends(pair.id, nodeId)
        .map(id => nodeName(id))
        .join(' → ')}`
    : pair.pair;
  const ready = readyOf(pair);
  // 受影响的条数取当前挂在未就绪隧道上的业务流。按事件里的 affected_sessions 累加会重复
  // 计数——同一批流会在 READY→SUSPECT 和 SUSPECT→DEAD 上各报一次。
  const hung = sessionsOf(pair.workers.filter(w => w.state !== 'READY'));
  const suspect = pair.workers.filter(w => w.state === 'SUSPECT').length;
  const dead = pair.workers.filter(w => w.state === 'DEAD').length;

  if (verdict === 'bad')
    return (
      <p className="rt-dx bad">
        <b>{who}</b> 当前无可用隧道{hung ? `；${hung} 条业务流仍由非就绪隧道承载` : ''}。
      </p>
    );

  if (suspect || dead)
    return (
      <p className="rt-dx">
        <b>{who}</b> {suspect ? `${suspect} 条隧道等待健康确认` : ''}
        {suspect && dead ? '，' : ''}
        {dead ? `${dead} 条隧道已关闭` : ''}；当前可用 {ready} 条隧道。
      </p>
    );

  return null;
}

/** 状态事件。序号不连续即在时间线里标出缺口：上游只保留最近 256 条尾部，
 *  xray 重启后序号从头开始，两种情况下中间那段都读不到。 */
function EventLog({ report, nodeName }: { report: Report; nodeName: (id: string) => string }) {
  const shown = report.events.slice(-SHOWN_EVENTS).reverse();
  if (shown.length === 0) return <p className="note rvh-none">没有事件。</p>;
  const label = (pair: string) => {
    const id = parsePair(pair);
    return id ? `${id.chain} · ${nodeName(id.peer)}` : pair;
  };
  return (
    <div className="rvh-log">
      {shown.map((e, index) => {
        const older = shown[index + 1];
        const gap = older && e.sequence - older.sequence > 1;
        const tone = e.state === 'DEAD' ? 'bad' : e.state === 'SUSPECT' ? 'warn' : e.state === 'READY' ? 'ok' : '';
        return (
          <div key={`${report.boot_id}:${e.sequence}`}>
            <div className={`rvh-ev ${tone}`}>
              <time>
                {clock(e.at_unix_ms)}
                <u>#{e.sequence}</u>
              </time>
              <div>
                <b>
                  {label(e.pair)} · {e.worker_id.slice(-8)}
                </b>{' '}
                <span className="rvh-arrow">
                  {e.from} → {e.state}
                </span>
                <i>
                  {e.reason}
                  {e.affected_sessions ? ` · 受影响业务流 ${e.affected_sessions}` : ''}
                </i>
              </div>
            </div>
            {gap && (
              <p className="rvh-gap">
                缺 {e.sequence - older.sequence - 1} 条 · #{older.sequence + 1}–#{e.sequence - 1}{' '}
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
