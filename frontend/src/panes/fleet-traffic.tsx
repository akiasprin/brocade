import { useMemo, useState, type CSSProperties, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';

import {
  fetchNodes,
  fetchUsers,
  REALTIME_USER_TRAFFIC_PROTOCOL_VERSION,
  type NodeAgentStateItem,
  type UserListItem,
} from '../api';
import { navigate } from '../forge/route';
import type { NodeRealtimeEvent } from '../node-realtime';
import { NodeTrafficDialog } from '../node-traffic';
import { useFleetTrafficSource, type FleetTrafficNode, type FleetTrafficSnapshot } from '../fleet-traffic-source';
import { PARTIAL_TEXT, PHASE_TEXT, RATE_SUM_NOTE, type TrafficPhase } from '../user-traffic-phase';
import { ErrorBox } from '../ui/bits';
import { shareOf } from '../ui/format';
import { PanelLoading } from '../ui/loading';
import { observeBpsReadingParts } from '../ui/observe-chart';
import { RegionFlag } from '../ui/region-flag';
import { GeneratedUserAvatar } from '../ui/user-avatar';
import { HEAT_STEPS, heatOf, type HeatKind } from '../ui/traffic-heat';

/* 用量页的「机队实时流量」：每台机器每秒上报的用户速率拼成「用户 × 机器」矩阵。
 * 左侧读数栏与月度页签同一骨架；右侧热力矩阵按地区成组排开整个机队，一格只表达四种事实：
 * 有速率（按固定十进档上色）、已采集且为 0、授权了但这台机器给不出当前读数、未授权（空白）。
 * 机器不多时列宽足够，格子直接写读数；窄屏改为按用户的流向列表。 */

const MIN_FRESH_MILLIS = 10_000;
/** 不超过这么多台机器时列宽放得下读数，格子写出数值，列名横排。 */
const WIDE_MACHINES = 6;
/** 超过这么多台机器时，较窄的卡片里读数栏移到矩阵上方；超过 PACKED_MACHINES 台在任何宽度下都如此。 */
const DENSE_MACHINES = 12;
const PACKED_MACHINES = 28;
/** 超过这么多位用户时只展开近期有流量的行，其余收起成一行说明。 */
const CROWD_USERS = 24;
/** 最后一次有流量之后仍算活跃的时长：突发流量之间的空档不让行在列表里进出。 */
const ACTIVE_HOLD_MILLIS = 30_000;
/** 窄屏流向列表每位用户列出的机器数。 */
const FLOW_PREVIEW = 3;
/** 窄屏空闲用户名单最多写出的名字数。 */
const IDLE_NAMES = 12;
const CONNECTING_NAMES = 4;

type CellState = 'rate' | 'idle' | 'unknown' | 'not-entitled';

export interface FleetTrafficCell {
  state: CellState;
  down: number;
  up: number;
  partial: boolean;
}

/** 这台机器的报告能否代表它的全部授权用户；与机器详情、用户详情的实时流量卡同一套阶段。 */
export type FleetMachinePhase = Extract<
  TrafficPhase,
  'ready' | 'pending' | 'offline' | 'unsupported' | 'unavailable' | 'disabled' | 'forwarding-unsupported'
>;

export interface FleetTrafficMatrixNode {
  id: string;
  name: string;
  country: string | null;
  rx: number | null;
  tx: number | null;
  current: boolean;
  complete: boolean;
  phase: FleetMachinePhase;
  /** 报告已就绪但不完整：已上报的行照常显示，没出现的授权用户不能当作 0。 */
  partial: boolean;
  /** 授权到这台机器的用户数。 */
  entitled: number;
  /** 当前在这台机器上有流量的用户数。 */
  active: number;
}

export interface FleetTrafficMatrixUser {
  key: string;
  tenantId: string;
  userId: string;
  cells: Map<string, FleetTrafficCell>;
  down: number;
  up: number;
  /** 授权到的机器数。 */
  entitled: number;
  /** 当前有流量的机器数。 */
  active: number;
  /** 授权了但没有当前读数的机器数。 */
  unknown: number;
}

export interface FleetTrafficMatrix {
  nodes: FleetTrafficMatrixNode[];
  users: FleetTrafficMatrixUser[];
  nodeTotals: Map<string, { down: number; up: number }>;
  nicRx: number;
  nicTx: number;
  /** 网卡读数有效、计入 nicRx / nicTx 的机器数。 */
  nicNodes: number;
  userDown: number;
  userUp: number;
  activeUsers: number;
  currentNodes: number;
}

const userKey = (tenantId: string, userId: string) => `${tenantId}/${userId}`;
const finiteRate = (value: unknown): value is number =>
  typeof value === 'number' && Number.isFinite(value) && value >= 0;
const nameOrder = new Intl.Collator('zh-CN', { numeric: true });

function latest(node: FleetTrafficNode) {
  return node.samples.at(-1) ?? null;
}

/** Freshness is judged per machine on the Console receipt axis; another machine's newer frame
 * says nothing about this one. */
function machinePhase(
  enabled: boolean,
  live: FleetTrafficNode | undefined,
  event: NodeRealtimeEvent | null,
  nowMillis: number,
  agentProtocol: number | null | undefined,
): { phase: FleetMachinePhase; partial: boolean } {
  if (!enabled) return { phase: 'disabled', partial: false };
  if (!live || !event) return { phase: 'pending', partial: false };
  const freshness = Math.max(MIN_FRESH_MILLIS, (live?.interval_secs ?? 1) * 3_000);
  if (!live.connected || !live.active || nowMillis - event.received_at_unix_millis > freshness) {
    return { phase: 'offline', partial: false };
  }
  const report = event.sample.user_traffic;
  // The first frame can contain NIC rates before the user-rate demand reaches the Agent.
  // Absence proves a missing capability only when its reported protocol is actually too old.
  if (!report) {
    return {
      phase:
        agentProtocol != null && agentProtocol < REALTIME_USER_TRAFFIC_PROTOCOL_VERSION ? 'unsupported' : 'pending',
      partial: false,
    };
  }
  if (report.state === 'unavailable') return { phase: 'unavailable', partial: false };
  if (report.state !== 'ready') return { phase: 'pending', partial: false };
  if (report.forwarding_expected && !report.forwarding_supported) {
    return { phase: 'forwarding-unsupported', partial: true };
  }
  return { phase: 'ready', partial: !report.all_users || !!report.truncated || !!report.has_gap };
}

/** Columns are grouped by the machine's public IPv4 region so a large fleet reads as a few runs
 * instead of one long strip: bigger regions first, machines by name inside a region, and
 * machines without a region last. */
function orderByRegion(nodes: FleetTrafficMatrixNode[]): FleetTrafficMatrixNode[] {
  const regions = new Map<string, FleetTrafficMatrixNode[]>();
  for (const node of nodes) {
    const region = node.country ?? '';
    regions.set(region, [...(regions.get(region) ?? []), node]);
  }
  return [...regions.entries()]
    .sort(
      ([left, a], [right, b]) =>
        Number(left === '') - Number(right === '') || b.length - a.length || left.localeCompare(right),
    )
    .flatMap(([, members]) =>
      members.sort((left, right) => nameOrder.compare(left.name, right.name) || left.id.localeCompare(right.id)),
    );
}

/** Pure cross-product builder. Missing rows become zero only after that individual machine has
 * supplied a fresh, complete `all_users` report; another machine's newer frame cannot certify it.
 * Users with no grant on any visible machine and no current rate have nothing to show and are
 * left out. */
export function buildFleetTrafficMatrix(
  payload: FleetTrafficSnapshot,
  nodeList: NodeAgentStateItem[],
  userList: UserListItem[],
  nowMillis: number,
): FleetTrafficMatrix {
  const liveNodes = new Map(payload.nodes.map(node => [node.node_id, node]));
  const expected = new Map(
    payload.expected.map(node => [
      node.node_id,
      new Set(node.users.map(user => userKey(user.tenant_id, user.user_id))),
    ]),
  );
  const users = new Map<string, { tenantId: string; userId: string }>();
  for (const user of userList) {
    users.set(userKey(user.tenant_id, user.id), { tenantId: user.tenant_id, userId: user.id });
  }
  for (const node of payload.expected) {
    for (const user of node.users) {
      users.set(userKey(user.tenant_id, user.user_id), { tenantId: user.tenant_id, userId: user.user_id });
    }
  }
  for (const node of payload.nodes) {
    for (const rate of latest(node)?.sample.user_traffic?.users ?? []) {
      users.set(userKey(rate.tenant_id, rate.user_id), { tenantId: rate.tenant_id, userId: rate.user_id });
    }
  }

  let nicRx = 0;
  let nicTx = 0;
  let nicNodes = 0;
  let currentNodes = 0;
  const quality = new Map<string, { complete: boolean; rates: Map<string, { down: number; up: number }> }>();
  const activeNodeStates = new Map(
    nodeList.filter(node => node.lifecycle_phase === 'active').map(node => [node.node_id, node]),
  );
  const knownNodes = new Set(activeNodeStates.keys());
  for (const node of payload.nodes) knownNodes.add(node.node_id);

  const nodes = orderByRegion(
    [...knownNodes].map((id): FleetTrafficMatrixNode => {
      const state = activeNodeStates.get(id);
      const live = liveNodes.get(id);
      const event = live ? latest(live) : null;
      const { phase, partial } = machinePhase(
        payload.policy.enabled,
        live,
        event,
        nowMillis,
        state?.agent_protocol_version,
      );
      const current = phase !== 'offline' && phase !== 'disabled' && event !== null;
      const report = current ? event?.sample.user_traffic : null;
      const complete = phase === 'ready' && !partial;
      const rates = new Map<string, { down: number; up: number }>();
      for (const rate of report?.users ?? []) {
        const key = userKey(rate.tenant_id, rate.user_id);
        const value = rates.get(key) ?? { down: 0, up: 0 };
        value.down += rate.downlink_bytes_per_sec;
        value.up += rate.uplink_bytes_per_sec;
        rates.set(key, value);
      }
      quality.set(id, { complete, rates });
      const sample = current && event && !event.sample.has_gap ? event.sample : null;
      const rx = sample && finiteRate(sample.rx_bytes_per_sec) ? sample.rx_bytes_per_sec : null;
      const tx = sample && finiteRate(sample.tx_bytes_per_sec) ? sample.tx_bytes_per_sec : null;
      if (rx !== null && tx !== null) {
        nicRx += rx;
        nicTx += tx;
        nicNodes += 1;
      }
      if (current && report?.state === 'ready') currentNodes += 1;
      return {
        id,
        name: state?.name || id,
        country: state?.public_ipv4_country?.trim().toUpperCase() || null,
        rx,
        tx,
        current,
        complete,
        phase,
        partial,
        entitled: expected.get(id)?.size ?? 0,
        active: 0,
      };
    }),
  );

  const nodeTotals = new Map(nodes.map(node => [node.id, { down: 0, up: 0 }]));
  let userDown = 0;
  let userUp = 0;
  let activeUsers = 0;
  const rows = [...users.entries()]
    .map(([key, user]): FleetTrafficMatrixUser => {
      const cells = new Map<string, FleetTrafficCell>();
      let down = 0;
      let up = 0;
      let entitledCount = 0;
      let activeCount = 0;
      let unknownCount = 0;
      for (const node of nodes) {
        const entitled = expected.get(node.id)?.has(key) ?? false;
        const nodeQuality = quality.get(node.id);
        const rate = nodeQuality?.rates.get(key);
        // A projected rate is authoritative even if the separately fetched entitlement snapshot
        // is briefly older. The Console has already applied the administrator's tenant boundary.
        const cell: FleetTrafficCell = rate
          ? { state: 'rate', down: rate.down, up: rate.up, partial: !nodeQuality?.complete }
          : !entitled
            ? { state: 'not-entitled', down: 0, up: 0, partial: false }
            : nodeQuality?.complete
              ? { state: 'idle', down: 0, up: 0, partial: false }
              : { state: 'unknown', down: 0, up: 0, partial: false };
        cells.set(node.id, cell);
        if (entitled) entitledCount += 1;
        if (cell.state === 'unknown') unknownCount += 1;
        if (cell.state === 'rate') {
          down += cell.down;
          up += cell.up;
          const total = nodeTotals.get(node.id)!;
          total.down += cell.down;
          total.up += cell.up;
          if (cell.down + cell.up > 0) {
            activeCount += 1;
            node.active += 1;
          }
        }
      }
      if (down + up > 0) activeUsers += 1;
      userDown += down;
      userUp += up;
      return {
        key,
        tenantId: user.tenantId,
        userId: user.userId,
        cells,
        down,
        up,
        entitled: entitledCount,
        active: activeCount,
        unknown: unknownCount,
      };
    })
    .filter(user => user.entitled > 0 || [...user.cells.values()].some(cell => cell.state === 'rate'))
    .sort((left, right) => nameOrder.compare(left.userId, right.userId) || left.key.localeCompare(right.key));

  return {
    nodes,
    users: rows,
    nodeTotals,
    nicRx,
    nicTx,
    nicNodes,
    userDown,
    userUp,
    activeUsers,
    currentNodes,
  };
}

/* ── 读数 ── */

type CellKind = HeatKind | 'unknown' | 'na';

function cellKind(cell: FleetTrafficCell): CellKind {
  if (cell.state === 'not-entitled') return 'na';
  if (cell.state === 'unknown') return 'unknown';
  return heatOf(cell.down + cell.up);
}

/** 能给出用户速率的机器：就绪（可能不完整）或只缺转发统计。 */
const reporting = (node: FleetTrafficMatrixNode) =>
  node.current && (node.phase === 'ready' || node.phase === 'forwarding-unsupported');

/** 成对读数按较大者取同一档单位，与实时流量卡一致。 */
function rateText(value: number, reference = value) {
  if (value === 0) return '0';
  const reading = observeBpsReadingParts(value * 8, reference * 8);
  return `${reading.number} ${reading.unit}`;
}

function pairText(down: number, up: number) {
  const reference = Math.max(down, up);
  return `下行 ${rateText(down, reference)} · 上行 ${rateText(up, reference)}`;
}

/** 这台机器为什么给不出（完整的）当前读数；能完整代表授权用户时为 null。 */
function machineIssue(node: FleetTrafficMatrixNode): string | null {
  if (node.phase === 'ready') return node.partial ? PARTIAL_TEXT : null;
  return PHASE_TEXT[node.phase];
}

function cellText(cell: FleetTrafficCell, node: FleetTrafficMatrixNode) {
  if (cell.state === 'not-entitled') return '未授权到这台机器';
  if (cell.state === 'unknown') return machineIssue(node) ?? '没有当前读数';
  if (cell.state === 'idle' || cell.down + cell.up === 0) return '已采集，当前没有流量';
  return `${pairText(cell.down, cell.up)}${cell.partial ? ` · ${PARTIAL_TEXT}` : ''}`;
}

function topFlow(matrix: FleetTrafficMatrix) {
  let best: { user: FleetTrafficMatrixUser; node: FleetTrafficMatrixNode; cell: FleetTrafficCell } | null = null;
  for (const user of matrix.users) {
    for (const node of matrix.nodes) {
      const cell = user.cells.get(node.id)!;
      if (cell.state !== 'rate' || cell.down + cell.up === 0) continue;
      if (!best || cell.down + cell.up > best.cell.down + best.cell.up) best = { user, node, cell };
    }
  }
  return best;
}

type BoardFocus =
  { kind: 'cell'; user: string; node: string } | { kind: 'user'; user: string } | { kind: 'node'; node: string };

const focusKey = (focus: BoardFocus | null) =>
  !focus
    ? ''
    : focus.kind === 'cell'
      ? `c\n${focus.user}\n${focus.node}`
      : focus.kind === 'user'
        ? `u\n${focus.user}`
        : `n\n${focus.node}`;
const carriesTraffic = (cell: FleetTrafficCell) => cell.state === 'rate' && cell.down + cell.up > 0;

function Readout({ matrix, focus }: { matrix: FleetTrafficMatrix; focus: BoardFocus | null }) {
  const user = focus && focus.kind !== 'node' ? matrix.users.find(row => row.key === focus.user) : undefined;
  const node = focus && focus.kind !== 'user' ? matrix.nodes.find(column => column.id === focus.node) : undefined;
  if (focus?.kind === 'cell' && user && node) {
    return (
      <>
        <b>{user.userId}</b> → {node.name} · {cellText(user.cells.get(node.id)!, node)}
      </>
    );
  }
  if (focus?.kind === 'user' && user) {
    const total = user.down + user.up;
    return (
      <>
        <b>{user.userId}</b> · {total > 0 ? pairText(user.down, user.up) : '当前空闲'} · {user.active} 台有流量 /{' '}
        {user.entitled} 台已授权{user.unknown > 0 ? ` · ${user.unknown} 台无读数` : ''}
      </>
    );
  }
  if (focus?.kind === 'node' && node) {
    const total = matrix.nodeTotals.get(node.id)!;
    const issue = machineIssue(node);
    const nicPair = Math.max(node.rx ?? 0, node.tx ?? 0);
    return (
      <>
        <b>{node.name}</b> ·{' '}
        {reporting(node)
          ? `用户 ${pairText(total.down, total.up)} · ${node.active} 位活跃 / ${node.entitled} 位已授权`
          : issue}
        {reporting(node) && issue ? ` · ${issue}` : ''}
        {node.rx !== null && node.tx !== null
          ? ` · 网卡收 ${rateText(node.rx, nicPair)} / 发 ${rateText(node.tx, nicPair)}`
          : ''}
      </>
    );
  }
  const top = topFlow(matrix);
  if (top) {
    return (
      <>
        当前最高 · <b>{top.user.userId}</b> → {top.node.name} · {pairText(top.cell.down, top.cell.up)}
      </>
    );
  }
  return <>{matrix.currentNodes > 0 ? '当前没有用户流量' : '没有机器提供当前读数'}</>;
}

/* ── 读数栏 ── */

function FleetLedger({ matrix, status }: { matrix: FleetTrafficMatrix; status: ReactNode }) {
  const down = matrix.userDown;
  const up = matrix.userUp;
  const total = down + up;
  const hero = observeBpsReadingParts(total * 8);
  const pair = Math.max(down, up);
  const topUser = matrix.users.reduce<FleetTrafficMatrixUser | null>(
    (best, user) => (user.down + user.up > (best ? best.down + best.up : 0) ? user : best),
    null,
  );
  const topNode = matrix.nodes.reduce<FleetTrafficMatrixNode | null>((best, node) => {
    const value = matrix.nodeTotals.get(node.id)!;
    const bestValue = best ? matrix.nodeTotals.get(best.id)! : { down: 0, up: 0 };
    return value.down + value.up > bestValue.down + bestValue.up ? node : best;
  }, null);
  const totalOf = (value: { down: number; up: number }) => rateText(value.down + value.up);
  const nicPair = Math.max(matrix.nicRx, matrix.nicTx);
  const missing = matrix.nodes.filter(node => !reporting(node)).map(node => node.name);

  return (
    <section className="usage-ledger flt-ledger" aria-label="机队当前速率">
      <div className="user-usage-hero">
        <span className="user-usage-hero-label" title={RATE_SUM_NOTE}>
          最近速率合计
        </span>
        <strong className="user-usage-hero-value">
          {total > 0 ? hero.number : '0'} <small>{hero.unit}</small>
        </strong>
        <span className="rut-hero-sub">
          <b>{matrix.activeUsers}</b> 位活跃 · <b>{matrix.users.length}</b> 位已授权
        </span>
        {status}
      </div>
      <div className="usage-compose">
        <div className="usage-split" aria-hidden="true">
          {down > 0 && <i className="down" style={{ flexGrow: down }} />}
          {up > 0 && <i className="up" style={{ flexGrow: up }} />}
        </div>
        <dl className="usage-io">
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{rateText(down, pair)}</dd>
            <dd className="usage-share">{total > 0 ? shareOf(down, total) : ''}</dd>
          </div>
          <div className="up">
            <dt>
              <i aria-hidden="true" />
              上行
            </dt>
            <dd>{rateText(up, pair)}</dd>
            <dd className="usage-share">{total > 0 ? shareOf(up, total) : ''}</dd>
          </div>
        </dl>
      </div>
      <dl className="usage-facts">
        <div>
          <dt>最高用户</dt>
          <dd title={topUser?.userId}>
            {topUser ? (
              <>
                {topUser.userId} · <span className="usage-nobreak">{totalOf(topUser)}</span>
              </>
            ) : (
              '—'
            )}
          </dd>
        </div>
        <div>
          <dt>最忙机器</dt>
          <dd title={topNode ? `${topNode.name}（按用户速率）` : undefined}>
            {topNode ? (
              <>
                {topNode.name} · <span className="usage-nobreak">{totalOf(matrix.nodeTotals.get(topNode.id)!)}</span>
              </>
            ) : (
              '—'
            )}
          </dd>
        </div>
        <div>
          <dt>网卡收 / 发</dt>
          <dd
            title={`${matrix.nicNodes} 台机器的网卡接收 / 发送合计，含中转与系统流量；与用户速率不是同一口径，不能直接相减。`}
          >
            {matrix.nicNodes > 0 ? (
              <>
                <span className="usage-nobreak">{rateText(matrix.nicRx, nicPair)}</span> /{' '}
                <span className="usage-nobreak">{rateText(matrix.nicTx, nicPair)}</span>
              </>
            ) : (
              '—'
            )}
          </dd>
        </div>
        <div>
          <dt>有读数</dt>
          <dd title={missing.length > 0 ? `没有完整读数：${missing.join('、')}` : '每台机器独立判定'}>
            {matrix.currentNodes} / {matrix.nodes.length} 台
          </dd>
        </div>
      </dl>
    </section>
  );
}

/* ── 热力矩阵 ── */

function HeatKey({ matrix, rows }: { matrix: FleetTrafficMatrix; rows: readonly FleetTrafficMatrixUser[] }) {
  const kinds = new Set(rows.flatMap(user => [...user.cells.values()].map(cellKind)));
  return (
    <span className="flt-key" aria-hidden="true">
      <span className="flt-key-item">
        <i className="idle" />
        空闲
      </span>
      {HEAT_STEPS.map(step => (
        <span key={step.kind} className="flt-key-item">
          <i className={step.kind} />
          {step.label}
        </span>
      ))}
      {kinds.has('unknown') && (
        <span className="flt-key-item">
          <i className="unknown" />
          无读数
        </span>
      )}
      {matrix.nodes.some(node => node.partial) && (
        <span className="flt-key-item">
          <i className="partial" />
          报告不完整
        </span>
      )}
      {kinds.has('na') && <span className="flt-key-item">空白：未授权</span>}
    </span>
  );
}

function FleetBoard({
  matrix,
  rows,
  hidden,
  expanded,
  onExpand,
  allowDetails,
  onOpenNode,
}: {
  matrix: FleetTrafficMatrix;
  rows: readonly FleetTrafficMatrixUser[];
  hidden: number;
  expanded: boolean;
  onExpand: (value: boolean) => void;
  allowDetails: boolean;
  onOpenNode: (node: FleetTrafficMatrixNode) => void;
}) {
  const [focus, setFocus] = useState<BoardFocus | null>(null);
  const wide = matrix.nodes.length <= WIDE_MACHINES;
  // The first machine of each region carries the flag; every later region also opens with a gap.
  const regionStarts = matrix.nodes
    .filter((node, index) => index === 0 || node.country !== matrix.nodes[index - 1].country)
    .map(node => node.id);
  const firstOfRegion = new Set(regionStarts);
  const groupStart = new Set(regionStarts.slice(1));
  const hotUser = focus && focus.kind !== 'node' ? focus.user : null;
  const hotNode = focus && focus.kind !== 'user' ? focus.node : null;
  const focusedUser = hotUser ? matrix.users.find(user => user.key === hotUser) : undefined;
  // Linking: a focused user lights the machines they are using, a focused machine its users.
  const litNodes = new Set(
    focusedUser
      ? matrix.nodes.filter(node => carriesTraffic(focusedUser.cells.get(node.id)!)).map(node => node.id)
      : [],
  );
  const litUsers = new Set(
    hotNode && focus?.kind === 'node'
      ? rows.filter(user => carriesTraffic(user.cells.get(hotNode)!)).map(user => user.key)
      : [],
  );
  const style = {
    '--flt-columns': matrix.nodes.length,
    '--flt-gaps': groupStart.size,
  } as CSSProperties;
  const columnClass = (node: FleetTrafficMatrixNode) =>
    `${groupStart.has(node.id) ? ' g' : ''}${hotNode === node.id ? ' hot' : ''}`;

  const pick = (target: EventTarget | null) => {
    if (!(target instanceof Element)) return;
    const row = target.closest<HTMLElement>('tr[data-user]');
    const cell = target.closest<HTMLElement>('td[data-node]');
    const head = target.closest<HTMLElement>('th[data-node]');
    // The row header and the row total describe the user; a totals-row cell describes its machine.
    if (row && cell) setFocus({ kind: 'cell', user: row.dataset.user!, node: cell.dataset.node! });
    else if (row) setFocus({ kind: 'user', user: row.dataset.user! });
    else if (cell || head) setFocus({ kind: 'node', node: (cell ?? head)!.dataset.node! });
    else setFocus(null);
  };
  const release = (target: BoardFocus) =>
    setFocus(current => (focusKey(current) === focusKey(target) ? null : current));

  return (
    <section className={`flt-board${wide ? ' wide' : ''}`} aria-label="用户与机器的当前速率">
      <div className="flt-scroll">
        <table
          className="flt-grid"
          style={style}
          onPointerOver={event => pick(event.target)}
          onPointerLeave={event => {
            if (event.pointerType !== 'touch') setFocus(null);
          }}
        >
          <caption className="flt-sr">
            每格为该用户在该机器上的当前速率；已采集且为 0 记为空闲，机器没有当前读数记为无读数。
          </caption>
          <colgroup>
            <col className="flt-col-user" />
            {matrix.nodes.map(node => (
              <col key={node.id} className={`flt-col-node${groupStart.has(node.id) ? ' g' : ''}`} />
            ))}
            <col className="flt-col-sum" />
          </colgroup>
          <thead>
            <tr>
              <th className="flt-corner" scope="col">
                <span className="flt-corner-label">用户</span>
              </th>
              {matrix.nodes.map(node => {
                const issue = machineIssue(node);
                return (
                  <th
                    key={node.id}
                    scope="col"
                    data-node={node.id}
                    className={`flt-mhead${columnClass(node)}${reporting(node) ? '' : ' dim'}${node.partial ? ' partial' : ''}${
                      litNodes.has(node.id) ? ' lit' : ''
                    }`}
                  >
                    {firstOfRegion.has(node.id) && node.country && (
                      <span className="flt-mflag">
                        <RegionFlag code={node.country} square />
                      </span>
                    )}
                    {allowDetails ? (
                      <button
                        type="button"
                        className="flt-mlabel"
                        title={issue ? `${node.name} · ${issue}` : node.name}
                        aria-label={`${node.name}${issue ? `，${issue}` : ''}，查看机器用量`}
                        aria-haspopup="dialog"
                        onFocus={() => setFocus({ kind: 'node', node: node.id })}
                        onBlur={() => release({ kind: 'node', node: node.id })}
                        onClick={() => onOpenNode(node)}
                      >
                        <span>{node.name}</span>
                      </button>
                    ) : (
                      <span className="flt-mlabel" title={issue ? `${node.name} · ${issue}` : node.name}>
                        <span>{node.name}</span>
                      </span>
                    )}
                  </th>
                );
              })}
              <th className="flt-sum" scope="col">
                合计
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map(user => {
              const total = user.down + user.up;
              return (
                <tr
                  key={user.key}
                  data-user={user.key}
                  className={`${hotUser === user.key ? 'hot' : ''}${litUsers.has(user.key) ? ' lit' : ''}`}
                >
                  <th scope="row" className="flt-uhead">
                    {allowDetails ? (
                      <button
                        type="button"
                        className="flt-ubtn"
                        title={user.userId}
                        onFocus={() => setFocus({ kind: 'user', user: user.key })}
                        onBlur={() => release({ kind: 'user', user: user.key })}
                        onClick={() => navigate('users', { p: 'user', id: user.userId })}
                      >
                        <GeneratedUserAvatar id={user.userId} variant="mini" />
                        <span className="flt-uname">{user.userId}</span>
                      </button>
                    ) : (
                      <span className="flt-ubtn static" title={user.userId}>
                        <GeneratedUserAvatar id={user.userId} variant="mini" />
                        <span className="flt-uname">{user.userId}</span>
                      </span>
                    )}
                  </th>
                  {matrix.nodes.map(node => {
                    const cell = user.cells.get(node.id)!;
                    const kind = cellKind(cell);
                    const focused = focus?.kind === 'cell' && hotUser === user.key && hotNode === node.id;
                    return (
                      <td key={node.id} data-node={node.id} className={`flt-c${columnClass(node)}`}>
                        {allowDetails ? (
                          <button
                            type="button"
                            className="flt-cbox flt-machine-open"
                            aria-label={`${user.userId} 在 ${node.name}：${cellText(cell, node)}，查看机器用量`}
                            aria-haspopup="dialog"
                            onFocus={() => setFocus({ kind: 'cell', user: user.key, node: node.id })}
                            onBlur={() => release({ kind: 'cell', user: user.key, node: node.id })}
                            onClick={() => onOpenNode(node)}
                          >
                            <i
                              className={`flt-cell ${kind}${cell.partial ? ' partial' : ''}${focused ? ' focus' : ''}`}
                            />
                            {wide && kind !== 'na' && (
                              <span className="flt-cv" aria-hidden="true">
                                {kind === 'unknown' ? '—' : rateText(cell.down + cell.up)}
                              </span>
                            )}
                          </button>
                        ) : (
                          <span className="flt-cbox">
                            <i
                              className={`flt-cell ${kind}${cell.partial ? ' partial' : ''}${focused ? ' focus' : ''}`}
                            />
                            {wide && kind !== 'na' && (
                              <span className="flt-cv" aria-hidden="true">
                                {kind === 'unknown' ? '—' : rateText(cell.down + cell.up)}
                              </span>
                            )}
                          </span>
                        )}
                        <span className="flt-sr">{cellText(cell, node)}</span>
                      </td>
                    );
                  })}
                  <td className={`flt-sum${total > 0 ? '' : ' zero'}`}>
                    {total > 0 ? rateText(total) : user.unknown === user.entitled && user.entitled > 0 ? '—' : '0'}
                  </td>
                </tr>
              );
            })}
          </tbody>
          <tfoot>
            <tr>
              <th scope="row" className="flt-uhead flt-foot-label">
                机器合计
              </th>
              {matrix.nodes.map(node => {
                const total = matrix.nodeTotals.get(node.id)!;
                const kind: CellKind = reporting(node) ? heatOf(total.down + total.up) : 'unknown';
                return (
                  <td key={node.id} data-node={node.id} className={`flt-c${columnClass(node)}`}>
                    {allowDetails ? (
                      <button
                        type="button"
                        className="flt-cbox flt-machine-open"
                        aria-label={`${node.name} 机器合计：${
                          reporting(node) ? pairText(total.down, total.up) : (machineIssue(node) ?? '没有当前读数')
                        }，查看机器用量`}
                        aria-haspopup="dialog"
                        onFocus={() => setFocus({ kind: 'node', node: node.id })}
                        onBlur={() => release({ kind: 'node', node: node.id })}
                        onClick={() => onOpenNode(node)}
                      >
                        <i className={`flt-cell ${kind}`} />
                        {wide && (
                          <span className="flt-cv" aria-hidden="true">
                            {kind === 'unknown' ? '—' : rateText(total.down + total.up)}
                          </span>
                        )}
                      </button>
                    ) : (
                      <span className="flt-cbox">
                        <i className={`flt-cell ${kind}`} />
                        {wide && (
                          <span className="flt-cv" aria-hidden="true">
                            {kind === 'unknown' ? '—' : rateText(total.down + total.up)}
                          </span>
                        )}
                      </span>
                    )}
                    <span className="flt-sr">
                      {reporting(node) ? pairText(total.down, total.up) : (machineIssue(node) ?? '没有当前读数')}
                    </span>
                  </td>
                );
              })}
              <td className="flt-sum">{rateText(matrix.userDown + matrix.userUp)}</td>
            </tr>
          </tfoot>
        </table>
      </div>
      {(hidden > 0 || expanded) && (
        <div className="flt-fold">
          {rows.length === 0 && <span>当前没有活跃用户</span>}
          <button className="btn text sm" type="button" aria-expanded={expanded} onClick={() => onExpand(!expanded)}>
            {expanded ? '只看活跃用户' : `其余 ${hidden} 位当前空闲 · 全部显示`}
          </button>
        </div>
      )}
      <footer className="flt-foot">
        <p className="flt-readout">
          <Readout matrix={matrix} focus={focus} />
        </p>
        <HeatKey matrix={matrix} rows={rows} />
      </footer>
    </section>
  );
}

/* ── 窄屏：按用户的流向列表 ── */

function FleetList({
  matrix,
  rows,
  idle,
  allowDetails,
}: {
  matrix: FleetTrafficMatrix;
  rows: readonly FleetTrafficMatrixUser[];
  idle: readonly FleetTrafficMatrixUser[];
  allowDetails: boolean;
}) {
  const connecting = matrix.nodes.filter(node => node.phase === 'pending');
  const issues = matrix.nodes.filter(node => node.phase !== 'pending' && machineIssue(node) !== null);
  return (
    <section className="flt-list" aria-label="按用户的当前速率">
      {rows.length === 0 ? (
        <p className="flt-list-empty">{connecting.length > 0 ? '正在等待速率数据' : '当前没有用户流量'}</p>
      ) : (
        <ul className="flt-flows">
          {rows.map(user => {
            const total = user.down + user.up;
            const flows = matrix.nodes
              .map(node => ({ node, cell: user.cells.get(node.id)! }))
              .filter(({ cell }) => cell.state === 'rate' && cell.down + cell.up > 0)
              .sort((left, right) => right.cell.down + right.cell.up - (left.cell.down + left.cell.up));
            return (
              <li key={user.key}>
                {allowDetails ? (
                  <button
                    type="button"
                    className="flt-flow"
                    title={user.userId}
                    onClick={() => navigate('users', { p: 'user', id: user.userId })}
                  >
                    <GeneratedUserAvatar id={user.userId} variant="cell" />
                    <span className="flt-flow-who">
                      <b>{user.userId}</b>
                    </span>
                    <span className={`flt-flow-total${total > 0 ? '' : ' zero'}`}>
                      {total > 0 ? rateText(total) : '空闲'}
                    </span>
                    <span className="flt-flow-nodes">
                      {flows.length === 0
                        ? '当前没有流量'
                        : flows
                            .slice(0, FLOW_PREVIEW)
                            .map(({ node, cell }) => `${node.name} ${rateText(cell.down + cell.up)}`)
                            .join(' · ') +
                          (flows.length > FLOW_PREVIEW ? ` · 另 ${flows.length - FLOW_PREVIEW} 台` : '')}
                    </span>
                  </button>
                ) : (
                  <div className="flt-flow static" title={user.userId}>
                    <GeneratedUserAvatar id={user.userId} variant="cell" />
                    <span className="flt-flow-who">
                      <b>{user.userId}</b>
                    </span>
                    <span className={`flt-flow-total${total > 0 ? '' : ' zero'}`}>
                      {total > 0 ? rateText(total) : '空闲'}
                    </span>
                    <span className="flt-flow-nodes">
                      {flows.length === 0
                        ? '当前没有流量'
                        : flows
                            .slice(0, FLOW_PREVIEW)
                            .map(({ node, cell }) => `${node.name} ${rateText(cell.down + cell.up)}`)
                            .join(' · ') +
                          (flows.length > FLOW_PREVIEW ? ` · 另 ${flows.length - FLOW_PREVIEW} 台` : '')}
                    </span>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {idle.length > 0 && (
        <p className="flt-list-rest">
          <b>空闲 {idle.length} 位</b>
          {idle
            .slice(0, IDLE_NAMES)
            .map(user => user.userId)
            .join('、')}
          {idle.length > IDLE_NAMES ? ' 等' : ''}
        </p>
      )}
      {connecting.length > 0 && (
        <p className="flt-list-rest">
          <b>连接中：</b>
          <span title={connecting.map(node => node.name).join('、')}>
            {connecting
              .slice(0, CONNECTING_NAMES)
              .map(node => node.name)
              .join('、')}
            {connecting.length > CONNECTING_NAMES ? ` 等 ${connecting.length} 台` : ''}
          </span>
        </p>
      )}
      {issues.length > 0 && (
        <p className="flt-list-rest">
          <b>读数不完整 {issues.length} 台</b>
          {issues.map(node => `${node.name}（${machineIssue(node)}）`).join('、')}
        </p>
      )}
    </section>
  );
}

/* ── 面板 ── */

/** 每位用户最近一次有流量的 Console 时刻，按渲染期「保存上一次信息」的写法随新矩阵更新。 */
function useLastActive(matrix: FleetTrafficMatrix | null, now: number) {
  const [state, setState] = useState<{ matrix: FleetTrafficMatrix | null; seen: ReadonlyMap<string, number> }>({
    matrix: null,
    seen: new Map(),
  });
  if (matrix && matrix !== state.matrix) {
    const seen = new Map(state.seen);
    for (const user of matrix.users) if (user.down + user.up > 0) seen.set(user.key, now);
    setState({ matrix, seen });
    return seen;
  }
  return state.seen;
}

function FleetCockpit({
  matrix,
  now,
  status,
  allowMachineDetails,
  onOpenNode,
}: {
  matrix: FleetTrafficMatrix;
  now: number;
  status: ReactNode;
  allowMachineDetails: boolean;
  onOpenNode: (node: FleetTrafficMatrixNode) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const seen = useLastActive(matrix, now);
  const recent = (user: FleetTrafficMatrixUser) =>
    user.down + user.up > 0 || now - (seen.get(user.key) ?? Number.NEGATIVE_INFINITY) <= ACTIVE_HOLD_MILLIS;
  const crowd = matrix.users.length > CROWD_USERS;
  const boardRows = crowd && !expanded ? matrix.users.filter(recent) : matrix.users;
  const listRows = matrix.users.filter(recent);

  return (
    <div
      className={`flt-cockpit${
        matrix.nodes.length > PACKED_MACHINES ? ' dense packed' : matrix.nodes.length > DENSE_MACHINES ? ' dense' : ''
      }`}
    >
      <FleetLedger matrix={matrix} status={status} />
      {matrix.users.length === 0 ? (
        <p className="flt-empty">尚无用户授权到这些机器</p>
      ) : (
        <>
          <FleetBoard
            matrix={matrix}
            rows={boardRows}
            hidden={matrix.users.length - boardRows.length}
            expanded={expanded}
            onExpand={setExpanded}
            allowDetails={allowMachineDetails}
            onOpenNode={onOpenNode}
          />
          <FleetList
            matrix={matrix}
            rows={listRows}
            idle={matrix.users.filter(user => !recent(user) && user.unknown === 0)}
            allowDetails={allowMachineDetails}
          />
        </>
      )}
    </div>
  );
}

function TrafficSurface({ children }: { children: ReactNode }) {
  return (
    <section className="flt-panel" aria-label="机队实时流量">
      {children}
    </section>
  );
}

export function FleetTrafficTab({ allowMachineDetails = true }: { allowMachineDetails?: boolean }) {
  const [selectedNode, setSelectedNode] = useState<Pick<FleetTrafficMatrixNode, 'id' | 'name' | 'country'> | null>(
    null,
  );
  const source = useFleetTrafficSource();
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const users = useQuery({
    queryKey: ['users', false],
    queryFn: () => fetchUsers(false),
  });
  // The snapshot already carries machine IDs and projected users. Names enrich that surface later;
  // neither roster requests nor machines still establishing a rate baseline should delay it.
  const matrix = useMemo(
    () =>
      source.payload
        ? buildFleetTrafficMatrix(source.payload, nodes.data?.nodes ?? [], users.data?.users ?? [], source.now)
        : null,
    [source.payload, source.now, nodes.data, users.data],
  );

  const error = nodes.error ?? users.error;
  const unavailableError = (!nodes.data && nodes.error) || (!users.data && users.error);
  if (!source.payload && unavailableError) {
    return (
      <TrafficSurface>
        <ErrorBox error={unavailableError} />
      </TrafficSurface>
    );
  }
  if (!source.payload && source.failed) {
    return (
      <TrafficSurface>
        <ErrorBox error={new Error('无法读取机队实时流量，正在等待服务恢复')} />
      </TrafficSurface>
    );
  }
  if (!source.payload) {
    return (
      <div className="usage-tab-loading" aria-busy="true">
        <PanelLoading label="读取实时用量" />
      </div>
    );
  }
  if (!matrix) return <ErrorBox error={new Error('机队实时流量没有返回可用数据')} />;

  const enabled = source.payload.policy.enabled;
  const warn = enabled && source.failed;
  const connecting = matrix.currentNodes === 0 && matrix.nodes.some(node => node.phase === 'pending');
  const status = (
    <span className={`rut-status${warn ? ' warn' : ''}`} role="status">
      <i className={`node-lamp${warn ? ' warn' : !enabled || connecting ? ' idle' : ''}`} />
      {!enabled
        ? PHASE_TEXT.disabled
        : warn
          ? `${PHASE_TEXT.offline} · 保留最近读数`
          : connecting
            ? '连接中'
            : PHASE_TEXT.ready}
    </span>
  );
  return (
    <TrafficSurface>
      {error && <ErrorBox error={error} />}
      {enabled ? (
        <FleetCockpit
          matrix={matrix}
          now={source.now}
          status={status}
          allowMachineDetails={allowMachineDetails}
          onOpenNode={setSelectedNode}
        />
      ) : (
        <div className="flt-permission">
          {status}
          <p>实时观测已在设置中关闭。</p>
        </div>
      )}
      {allowMachineDetails && selectedNode && (
        <NodeTrafficDialog
          key={selectedNode.id}
          nodeId={selectedNode.id}
          name={selectedNode.name}
          country={selectedNode.country}
          onClose={() => setSelectedNode(null)}
        />
      )}
    </TrafficSurface>
  );
}
