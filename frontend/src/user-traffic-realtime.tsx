import { lazy } from './ui/lazy';
import { useMetricQuery } from './ui/metric-cadence';
import { memo, Suspense, useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
  fetchNodeTrafficHistory,
  fetchUserTrafficHistory,
  type NodeTrafficHistory,
  type SiteUsagePeriod,
  type TrafficTimeRange,
  type UserTrafficHistory,
} from './api';
import { useNodeRealtime, type RealtimeUserTrafficRate } from './node-realtime';
import { throughputAxis } from './panes/telemetry-format';
import { SegmentedControl } from './ui/bits';
import { bytes, shareOf } from './ui/format';
import { Icon, PanelTitle } from './ui/icons';
import { useNodeCountries } from './ui/node-name';
import { observeBpsReadingParts } from './ui/observe-chart';
import { RegionFlag } from './ui/region-flag';
import { sparklinePaths } from './ui/sparkline';
import { PARTIAL_TEXT, PHASE_TEXT, RATE_SUM_NOTE, type TrafficPhase } from './user-traffic-phase';
import { FRESH_MILLIS, useUserTrafficSource, type UserTrafficNode } from './user-traffic-source';

/* 用户实时流量卡（用量中的机器弹层 / 用户详情）。两路数据分开呈现，不拼接：
 * - 历史：从 Agent 记账窗口读取，每 30 秒刷新。机器弹层随统计区间汇总本机柱状图；
 *   用户详情保留近 1 小时的入口机器合计图和每台机器的走势。
 * - 当前：打开页面后按需订阅的每秒速率，以读数呈现（左侧合计加上下行占比条）。打开前没有秒级数据，
 *   画成时间序列会在打开时出现空图。
 * 出现过的行保留到离开页面：机器详情的 all 模式不上报速率为 0 的用户，按最新报告重建列表
 * 会让用户每秒出现又消失。默认按名称排列且不自动重排；选择数值排序后只更新读数，当前可见
 * 名次变化时提示用户确认重排，避免阅读中的卡片自己跳动。 */

const LazyThroughputChart = lazy(() =>
  import('./panes/telemetry').then(module => ({ default: module.ThroughputChart })),
);

const HOUR_SECS = 3_600;
const SLOT_SECS = 30;
const SLOTS = HOUR_SECS / SLOT_SECS;
/** 连续这么久没有流量才算空闲：突发流量之间的空档不改变行的样式。 */
const IDLE_MILLIS = 10_000;
const HISTORY_REFRESH_MILLIS = 30_000;
/** 大人群列表只展开到与左侧读数栏大致齐平；手机上再缩短首屏。 */
const CROWD_FOLD = 14;
const CROWD_FOLD_NARROW = 8;
const MORE_STEP = 60;
/** 与协议 MAX_REALTIME_USER_TRAFFIC_ROWS 一致，保留的用户行与每行来源身份均受此上限约束。 */
const RETAINED_LIMIT = 2_048;
const SPARK_HEIGHT = 30;
const SCOPE_NOTE = '统计入口 Xray 的接入与转发；中继不提供最终用户身份';
const NO_TARGETS: readonly string[] = [];

type TrafficSort = 'name' | 'down' | 'up' | 'hour';
const USER_SORTS: readonly { value: TrafficSort; label: string }[] = [
  { value: 'name', label: '名称' },
  { value: 'down', label: '下行' },
  { value: 'up', label: '上行' },
  { value: 'hour', label: '近 1 小时' },
];

/** 一个对象的当前速率（B/s）。null 表示最新报告给不出这个值：未就绪、被截断或未连接。 */
export interface LiveTrafficRow {
  key: string;
  name: string;
  detail: string;
  down: number | null;
  up: number | null;
  /** False keeps a last-known row visible without counting it as a current measurement. */
  current?: boolean;
  /** Console receipt time for this row, never the newest receipt of another machine. */
  measuredAt?: number;
  /** 用户详情：这台入口机器的实时通道未连接或数据已过期。 */
  offline?: boolean;
  /** 用户详情：该用户在这台机器上报告的接入点数。 */
  ingresses?: number;
  forwardingDown?: number;
  forwardingUp?: number;
  /** 机器详情：按来源分的当前速率，键为 `ingress:<接入点>` 或 `forwarding`，页面再映射到接入链。 */
  sources?: Readonly<Record<string, { down: number; up: number }>>;
}

interface RetainedRow extends LiveTrafficRow {
  lastActiveMs: number | null;
}

/** 一个对象近 1 小时的窗口字节，按 30 秒格对齐；null 是该格没有采集到窗口。 */
interface HourRow {
  key: string;
  tenant?: string;
  user?: string;
  down: (number | null)[];
  up: (number | null)[];
  hourDown: number;
  hourUp: number;
  lastActiveMs: number | null;
  unplaced?: boolean;
  forwarding?: boolean;
}

interface HourModel {
  rangeStart: number;
  rangeEnd: number;
  slots: number[];
  rows: Map<string, HourRow>;
  totalDown: (number | null)[];
  totalUp: (number | null)[];
  /** 全体对象的窗口平均速率峰值（B/s），各行走势的缩放下限由它决定。 */
  peak: number;
  /** 机器统计返回已记录总量；其曲线以已观测窗口的均值表示，不能反推累计量。 */
  recordedBytes?: number;
}

export interface CardRow {
  key: string;
  name: string;
  detail: string;
  down: number | null;
  up: number | null;
  offline: boolean;
  current?: boolean;
  measuredAt?: number;
  ingresses?: number;
  forwardingDown?: number;
  forwardingUp?: number;
  sources?: LiveTrafficRow['sources'];
  hour: HourRow | null;
  hourBytes: number;
  lastActiveMs: number | null;
}

/** 机器详情一项当前来源的显示：接入链（多个接入点合并）、端口转发，或已不在模型里的接入点。 */
export interface TrafficSource {
  key: string;
  name: string;
  country: string | null;
  muted?: boolean;
}

export const FORWARDING_SOURCE = 'forwarding';

/** 没有模型可查时的写法：接入点只写编号，转发写「端口转发」。 */
export function fallbackSource(key: string): TrafficSource {
  if (key === FORWARDING_SOURCE) return { key, name: '端口转发', country: null, muted: true };
  const ingress = key.startsWith('ingress:') ? key.slice('ingress:'.length) : key;
  return { key, name: ingress ? `接入点 ${ingress}` : '接入点未知', country: null, muted: true };
}

const userKey = (tenantId: string, userId: string) => `${tenantId}/${userId}`;
const slotOf = (unixSecs: number) => Math.floor(unixSecs / SLOT_SECS) * SLOT_SECS;

/** 一行报告的来源：转发规则合为一项，接入按接入点区分。 */
const sourceKey = (user: RealtimeUserTrafficRate) =>
  user.forwarding_rule_id !== undefined ? FORWARDING_SOURCE : `ingress:${user.ingress_id ?? ''}`;

/** 同一用户可能经由多个接入点；租户不同的同名用户是两个人。 */
export function aggregateUserTraffic(users: RealtimeUserTrafficRate[]): LiveTrafficRow[] {
  const grouped = new Map<
    string,
    LiveTrafficRow & { down: number; up: number; sources: Record<string, { down: number; up: number }> }
  >();
  for (const user of users) {
    const key = userKey(user.tenant_id, user.user_id);
    const row = grouped.get(key) ?? { key, name: user.user_id, detail: user.tenant_id, down: 0, up: 0, sources: {} };
    row.down += user.downlink_bytes_per_sec;
    row.up += user.uplink_bytes_per_sec;
    const source = (row.sources[sourceKey(user)] ??= { down: 0, up: 0 });
    source.down += user.downlink_bytes_per_sec;
    source.up += user.uplink_bytes_per_sec;
    if (user.forwarding_rule_id !== undefined) {
      row.forwardingDown = (row.forwardingDown ?? 0) + user.downlink_bytes_per_sec;
      row.forwardingUp = (row.forwardingUp ?? 0) + user.uplink_bytes_per_sec;
    }
    grouped.set(key, row);
  }
  return [...grouped.values()];
}

/** 旧版近 1 小时的窗口落在 30 秒格上。格内有窗口而没有字节是
 * 空闲，记 0；格内没有窗口是未采集，记 null。用户详情的合计跨机器相加，任一台采集到即算采集。 */
function legacyHourModel(history: UserTrafficHistory): HourModel {
  const last = slotOf(history.range_end_unix_secs);
  const slots = Array.from({ length: SLOTS }, (_, index) => last - (SLOTS - 1 - index) * SLOT_SECS);
  const indexOf = new Map(slots.map((slot, index) => [slot, index]));
  const collectedBy = new Map(
    history.nodes.map(node => [node.node_id, new Set(node.window_ends_unix_secs.map(slotOf))]),
  );
  const blank = (collected: Set<number> | undefined): (number | null)[] =>
    slots.map(slot => (collected?.has(slot) ? 0 : null));
  const rows = new Map<string, HourRow>();
  const rowFor = (key: string, nodeId: string) => {
    let row = rows.get(key);
    if (!row) {
      const collected = collectedBy.get(nodeId);
      row = { key, down: blank(collected), up: blank(collected), hourDown: 0, hourUp: 0, lastActiveMs: null };
      rows.set(key, row);
    }
    return row;
  };
  // 用户详情列出该用户的全部入口机器，近 1 小时没有流量的也列出。
  history.nodes.forEach(node => rowFor(node.node_id, node.node_id));
  for (const series of history.series) {
    const key = series.node_id;
    const row = rowFor(key, series.node_id);
    row.tenant = series.tenant_id;
    row.user = series.user_id;
    for (const [end, up, down] of series.points) {
      row.hourDown += down;
      row.hourUp += up;
      if (up + down > 0) row.lastActiveMs = Math.max(row.lastActiveMs ?? 0, end * 1000);
      const index = indexOf.get(slotOf(end));
      if (index === undefined) continue;
      row.down[index] = (row.down[index] ?? 0) + down;
      row.up[index] = (row.up[index] ?? 0) + up;
    }
  }
  const collectedAny = new Set([...collectedBy.values()].flatMap(set => [...set]));
  const totalDown: (number | null)[] = slots.map(slot => (collectedAny.has(slot) ? 0 : null));
  const totalUp = [...totalDown];
  let peak = 0;
  for (const row of rows.values()) {
    row.down.forEach((value, index) => {
      if (value !== null) totalDown[index] = (totalDown[index] ?? 0) + value;
      const up = row.up[index];
      if (value !== null || up !== null) peak = Math.max(peak, ((value ?? 0) + (up ?? 0)) / SLOT_SECS);
    });
    row.up.forEach((value, index) => {
      if (value !== null) totalUp[index] = (totalUp[index] ?? 0) + value;
    });
  }
  return {
    rangeStart: history.range_start_unix_secs,
    rangeEnd: history.range_end_unix_secs,
    slots,
    rows,
    totalDown,
    totalUp,
    peak,
  };
}

type Interval = readonly [number, number];
function mergedIntervals(intervals: readonly Interval[]): Interval[] {
  const sorted = intervals.filter(([start, end]) => end > start).sort((a, b) => a[0] - b[0]);
  const merged: [number, number][] = [];
  for (const [start, end] of sorted) {
    const last = merged.at(-1);
    if (last && start <= last[1]) last[1] = Math.max(last[1], end);
    else merged.push([start, end]);
  }
  return merged;
}

function activeIntervals(start: number, end: number, zeros: readonly Interval[]): Interval[] {
  const active: Interval[] = [];
  let cursor = start;
  for (const [zeroStart, zeroEnd] of zeros) {
    if (zeroEnd <= cursor || zeroStart >= end) continue;
    if (zeroStart > cursor) active.push([cursor, Math.min(zeroStart, end)]);
    cursor = Math.max(cursor, zeroEnd);
  }
  if (cursor < end) active.push([cursor, end]);
  return active;
}

function intersectIntervals(left: readonly Interval[], right: readonly Interval[]): Interval[] {
  const intersections: Interval[] = [];
  let i = 0,
    j = 0;
  while (i < left.length && j < right.length) {
    const start = Math.max(left[i][0], right[j][0]),
      end = Math.min(left[i][1], right[j][1]);
    if (end > start) intersections.push([start, end]);
    if (left[i][1] < right[j][1]) i++;
    else j++;
  }
  return intersections;
}

const overlap = (start: number, end: number, intervals: readonly Interval[]) =>
  intervals.reduce((sum, [a, b]) => sum + Math.max(0, Math.min(end, b) - Math.max(start, a)), 0);

/** Use actual measured intervals, not the report's endpoint bucket. Long cumulative windows
 * preserve byte totals and draw their average rate across every covered slot. Only confirmed
 * downtime is zero; uncovered time remains null. This is chart resampling, not ledger rewriting. */
export function hourModel(history: UserTrafficHistory): HourModel {
  if (history.nodes.every(node => node.windows === undefined)) return legacyHourModel(history);
  const rangeStart = history.range_start_unix_secs;
  const rangeEnd = history.range_end_unix_secs;
  const first = slotOf(rangeStart);
  const count = Math.ceil((rangeEnd - first) / SLOT_SECS);
  const slots = Array.from({ length: count }, (_, index) => first + index * SLOT_SECS);
  const bounds = slots.map(slot => [Math.max(slot, rangeStart), Math.min(slot + SLOT_SECS, rangeEnd)] as const);
  const zeros = new Map(
    history.nodes.map(node => [
      node.node_id,
      mergedIntervals((node.zero_windows ?? []).map(([a, b]) => [a / 1000, b / 1000])),
    ]),
  );
  const collected = new Map(
    history.nodes.map(node => [
      node.node_id,
      mergedIntervals([
        ...(node.windows ?? []),
        ...(node.forwarding_windows ?? []),
        ...(zeros.get(node.node_id) ?? []),
      ]),
    ]),
  );
  const measuredByRow = new Map<string, Interval[]>();
  const sourcesByRow = new Map<string, UserTrafficHistory['series']>();
  for (const series of history.series) {
    const key = series.node_id;
    const grouped = sourcesByRow.get(key) ?? [];
    grouped.push(series);
    sourcesByRow.set(key, grouped);
  }
  for (const [key, sources] of sourcesByRow) {
    const intervals: Interval[] = [];
    for (const node of history.nodes) {
      const relevant = sources.filter(source => source.node_id === node.node_id);
      if (relevant.length === 0) continue;
      const access = mergedIntervals([...(node.windows ?? []), ...(zeros.get(node.node_id) ?? [])]);
      const forwarding = mergedIntervals([...(node.forwarding_windows ?? []), ...(zeros.get(node.node_id) ?? [])]);
      const hasAccess = relevant.some(s => !s.forwarding),
        hasForwarding = relevant.some(s => s.forwarding);
      intervals.push(
        ...(hasAccess && hasForwarding ? intersectIntervals(access, forwarding) : hasForwarding ? forwarding : access),
      );
    }
    measuredByRow.set(key, mergedIntervals(intervals));
  }
  const blank = (nodeId: string, key: string) =>
    bounds.map(([start, end]) =>
      overlap(start, end, measuredByRow.get(key) ?? collected.get(nodeId) ?? []) >= end - start - 0.001 ? 0 : null,
    );
  const rows = new Map<string, HourRow>();
  const rowFor = (key: string, nodeId: string) => {
    let row = rows.get(key);
    if (!row) {
      row = { key, down: blank(nodeId, key), up: blank(nodeId, key), hourDown: 0, hourUp: 0, lastActiveMs: null };
      rows.set(key, row);
    }
    return row;
  };
  history.nodes.forEach(node => rowFor(node.node_id, node.node_id));
  for (const series of history.series) {
    const row = rowFor(series.node_id, series.node_id);
    row.tenant = series.tenant_id;
    row.user = series.user_id;
    row.forwarding ||= !!series.forwarding;
    row.hourUp += series.unplaced_uplink_bytes ?? 0;
    row.hourDown += series.unplaced_downlink_bytes ?? 0;
    row.unplaced ||= (series.unplaced_uplink_bytes ?? 0) + (series.unplaced_downlink_bytes ?? 0) > 0;
    const windows = series.windows ?? series.points.map(([end, up, down]) => [end - SLOT_SECS, end, up, down]);
    for (const [start, end, up, down] of windows) {
      if (end <= start) continue;
      const active = activeIntervals(start, end, zeros.get(series.node_id) ?? []);
      const duration = overlap(start, end, active);
      if (duration <= 0) continue;
      const visible = overlap(rangeStart, rangeEnd, active) / duration;
      row.hourUp += up * visible;
      row.hourDown += down * visible;
      if (up + down > 0) row.lastActiveMs = Math.max(row.lastActiveMs ?? 0, end * 1000);
      bounds.forEach(([a, b], index) => {
        const seconds = overlap(a, b, active);
        // Values retain the historical "bytes per 30 seconds" shape. The partial boundary
        // slots use their actual visible width, so conversion to bit/s stays correct.
        const weight = ((seconds / duration) * SLOT_SECS) / (b - a);
        if (row.up[index] !== null) row.up[index] += up * weight;
        if (row.down[index] !== null) row.down[index] += down * weight;
      });
    }
  }
  const totalDown: (number | null)[] = bounds.map(([a, b], index) =>
    (
      rows.size > 0
        ? [...rows.values()].some(row => row.down[index] !== null)
        : [...collected.values()].some(intervals => overlap(a, b, intervals) >= b - a - 0.001)
    )
      ? 0
      : null,
  );
  const totalUp = [...totalDown];
  let peak = 0;
  for (const row of rows.values()) {
    row.down.forEach((down, index) => {
      const up = row.up[index];
      if (down !== null) totalDown[index] = (totalDown[index] ?? 0) + down;
      if (up !== null) totalUp[index] = (totalUp[index] ?? 0) + up;
      peak = Math.max(peak, ((down ?? 0) + (up ?? 0)) / SLOT_SECS);
    });
  }
  return { rangeStart, rangeEnd, slots, rows, totalDown, totalUp, peak };
}

/** 来源键持续保留，缺席项用零占位；是否能显示为零由报告完整性决定，名称继续由模型映射。 */
function retainSources(previous: LiveTrafficRow['sources'], current: LiveTrafficRow['sources']) {
  if (!previous) return current;
  const keys = [...new Set([...Object.keys(current ?? {}), ...Object.keys(previous)])].slice(0, RETAINED_LIMIT);
  return Object.fromEntries(keys.map(key => [key, current?.[key] ?? { down: 0, up: 0 }]));
}

/** 合并一份新报告。未就绪或未连接（null）的读数不覆盖最后一次有效数据；报告完整时，
 * 不在其中的已有行即为空闲。行数受协议上限约束，超出时先丢最久未活跃的。 */
export function mergeRetained(
  previous: ReadonlyMap<string, RetainedRow>,
  live: readonly LiveTrafficRow[],
  complete: boolean,
  at: number,
): Map<string, RetainedRow> {
  const next = new Map(previous);
  const seen = new Set<string>();
  for (const row of live) {
    seen.add(row.key);
    const before = previous.get(row.key);
    const known = row.down !== null && row.up !== null;
    const active = (row.down ?? 0) + (row.up ?? 0) > 0;
    next.set(row.key, {
      ...row,
      down: known ? row.down : (before?.down ?? null),
      up: known ? row.up : (before?.up ?? null),
      current: known && row.current !== false,
      measuredAt: known ? (row.measuredAt ?? at) : before?.measuredAt,
      ingresses: row.ingresses ?? before?.ingresses,
      forwardingDown: known ? row.forwardingDown : before?.forwardingDown,
      forwardingUp: known ? row.forwardingUp : before?.forwardingUp,
      sources: known ? retainSources(before?.sources, row.sources) : before?.sources,
      lastActiveMs: active ? (row.measuredAt ?? at) : (before?.lastActiveMs ?? null),
    });
  }
  for (const [key, row] of previous) {
    if (!seen.has(key)) {
      next.set(
        key,
        complete
          ? {
              ...row,
              down: 0,
              up: 0,
              current: true,
              measuredAt: at,
              forwardingDown: row.forwardingDown === undefined ? undefined : 0,
              forwardingUp: row.forwardingUp === undefined ? undefined : 0,
              sources: retainSources(row.sources, undefined),
            }
          : { ...row, current: false },
      );
    }
  }
  if (next.size > RETAINED_LIMIT) {
    const evict = [...next.values()]
      .sort((a, b) => (a.lastActiveMs ?? 0) - (b.lastActiveMs ?? 0))
      .slice(0, next.size - RETAINED_LIMIT);
    evict.forEach(row => next.delete(row.key));
  }
  return next;
}

/** 渲染期按新报告调整保留的行（React 的「上一次渲染的信息」写法）：同一份报告只合并一次。 */
function useRetained(at: number | null, live: readonly LiveTrafficRow[], complete: boolean, revision: unknown = at) {
  const [state, setState] = useState<{ at: number | null; revision: unknown; rows: ReadonlyMap<string, RetainedRow> }>({
    at: null,
    revision: null,
    rows: new Map(),
  });
  if (at !== null && revision !== state.revision) {
    const next = { at, revision, rows: mergeRetained(state.rows, live, complete, at) };
    setState(next);
    return next;
  }
  return state;
}

const totalOf = (row: CardRow) => (row.down ?? 0) + (row.up ?? 0);
const nameOrder = new Intl.Collator('zh-CN', { numeric: true });
function compareRows(sort: TrafficSort) {
  return (a: CardRow, b: CardRow) => {
    const byName = nameOrder.compare(a.name, b.name) || a.key.localeCompare(b.key);
    if (sort === 'name') return byName;
    if (sort === 'hour') return b.hourBytes - a.hourBytes || byName;
    const known = Number(b.down !== null) - Number(a.down !== null);
    if (known) return known;
    const recent = (b.lastActiveMs ?? 0) - (a.lastActiveMs ?? 0);
    if (sort === 'up') return (b.up ?? 0) - (a.up ?? 0) || (b.down ?? 0) - (a.down ?? 0) || recent || byName;
    return (b.down ?? 0) - (a.down ?? 0) || (b.up ?? 0) - (a.up ?? 0) || recent || byName;
  };
}

interface ListOrder {
  keys: string[];
  at: number;
  sort: TrafficSort;
  /** 数值排序第一次拿到当前读数时可建立初始快照；之后实时变化必须由用户确认。 */
  measured?: boolean;
}

/** 下一份顺序；数值变化只更新读数，只有切换排序或用户确认时才移动已有卡片。 */
export function nextListOrder(
  order: ListOrder,
  rows: readonly CardRow[],
  sort: TrafficSort,
  resort: boolean,
  now: number,
): ListOrder {
  const known = new Set(order.keys);
  const added = rows.filter(row => !known.has(row.key)).map(row => row.key);
  const present = new Set(rows.map(row => row.key));
  const kept = order.keys.filter(key => present.has(key));
  const removed = kept.length !== order.keys.length;
  const measured = rows.some(row => row.down !== null && row.up !== null);
  const shouldSort =
    resort ||
    order.sort !== sort ||
    (order.keys.length === 0 && rows.length > 0) ||
    (sort === 'name' && added.length > 0) ||
    (sort !== 'name' && measured && !order.measured);
  if (shouldSort) return { keys: [...rows].sort(compareRows(sort)).map(row => row.key), at: now, sort, measured };
  if (added.length > 0 || removed) return { ...order, keys: [...kept, ...added] };
  return order;
}

function useListOrder(rows: readonly CardRow[], sort: TrafficSort, now: number) {
  const [order, setOrder] = useState<ListOrder>({ keys: [], at: Number.NEGATIVE_INFINITY, sort });
  const next = nextListOrder(order, rows, sort, false, now);
  if (next !== order) setOrder(next);
  return {
    keys: next.keys,
    resort: () => setOrder(current => nextListOrder(current, rows, sort, true, now)),
  };
}

/** 位置变化的行从旧位置平移到新位置（FLIP）；只在顺序或可见集合变化后执行。返回挂在列表上的 ref。 */
function useFlip(signature: string) {
  const listRef = useRef<HTMLDivElement>(null);
  const previous = useRef(new Map<string, DOMRect>());
  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list) return;
    const reduce = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
    const current = new Map<string, DOMRect>();
    const moved: HTMLElement[] = [];
    for (const child of list.children) {
      if (!(child instanceof HTMLElement) || !child.dataset.key) continue;
      child.style.transition = 'none';
      child.style.transform = '';
      const box = child.getBoundingClientRect();
      current.set(child.dataset.key, box);
      const before = previous.current.get(child.dataset.key);
      if (!before || reduce) continue;
      const dx = before.left - box.left;
      const dy = before.top - box.top;
      if (Math.abs(dx) < 1 && Math.abs(dy) < 1) continue;
      child.style.transform = `translate(${dx}px, ${dy}px)`;
      moved.push(child);
    }
    previous.current = current;
    if (moved.length === 0) return;
    void list.offsetHeight;
    for (const child of moved) {
      child.style.transition = 'transform 320ms cubic-bezier(.2,.7,.2,1)';
      child.style.transform = '';
    }
  }, [signature]);
  return listRef;
}

/* ── 读数 ── */

/** 当前速率：bit/s，与机器详情吞吐图同一套阶梯；成对读数按较大者取同一档单位。 */
function Rate({ value, pair }: { value: number | null; pair: number }) {
  if (value === null) return <>—</>;
  if (value === 0) return <>0</>;
  const reading = observeBpsReadingParts(value * 8, pair * 8);
  return (
    <>
      {reading.number}
      <small>{reading.unit}</small>
    </>
  );
}

/** 读数栏里的速率是纯文本（与「用量与额度」读数栏一致），数值与单位之间留一个空格。 */
function rateText(value: number, pair: number) {
  if (value === 0) return '0';
  const reading = observeBpsReadingParts(value * 8, pair * 8);
  return `${reading.number} ${reading.unit}`;
}

function Bytes({ value }: { value: number }) {
  if (value === 0) return <>0</>;
  const [number, unit] = bytes(value).split(' ');
  return (
    <>
      {number}
      <small>{unit}</small>
    </>
  );
}

const idleLabel = (idleMillis: number) =>
  !Number.isFinite(idleMillis)
    ? '近 1 小时无流量'
    : idleMillis < 60_000
      ? '空闲'
      : idleMillis < 3_600_000
        ? `空闲 ${Math.floor(idleMillis / 60_000)} 分钟`
        : `空闲 ${Math.floor(idleMillis / 3_600_000)} 小时`;

/** 近 1 小时走势：每格一个窗口的平均速率，下行画面积、上行画细线（与用户详情合计图同一编码）；
 * 缺失的格断开，采集到而没有流量的时段只留底线。按本行峰值缩放，下限是全体峰值的 6%，
 * 零星流量不会被放大成满格。 */
const HourSpark = memo(function HourSpark({
  hour,
  peak,
  sharedScale = false,
}: {
  hour: HourRow | null;
  peak: number;
  sharedScale?: boolean;
}) {
  const paths = useMemo(() => {
    if (!hour) return null;
    const perSecond = (values: (number | null)[]) => values.map(value => (value === null ? null : value / SLOT_SECS));
    const down = perSecond(hour.down);
    const up = perSecond(hour.up);
    const own = Math.max(0, ...down.map(value => value ?? 0), ...up.map(value => value ?? 0));
    const scale = Math.max(sharedScale ? peak : own, sharedScale ? 0 : peak * 0.06, 1024) * 1.06;
    return {
      width: Math.max(1, down.length - 1),
      down: sparklinePaths(down, scale, SPARK_HEIGHT),
      up: sparklinePaths(up, scale, SPARK_HEIGHT),
    };
  }, [hour, peak, sharedScale]);
  return (
    <svg
      className="rut-spark"
      viewBox={`0 0 ${paths?.width ?? SLOTS - 1} ${SPARK_HEIGHT}`}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      {paths?.down.bases.map((path, index) => (
        <path key={`b${index}`} className="rut-spark-base" d={path} />
      ))}
      {paths?.down.areas.map((path, index) => (
        <path key={`a${index}`} className="rut-spark-area" d={path} />
      ))}
      {paths?.up.lines.map((path, index) => (
        <path key={`u${index}`} className="rut-spark-up" d={path} />
      ))}
      {paths?.down.lines.map((path, index) => (
        <path key={`d${index}`} className="rut-spark-line" d={path} />
      ))}
    </svg>
  );
});

/** 入口机器的身份牌，同机器详情页头的 .nd-idplate：地区旗铺满圆角方牌，国别取公网 IPv4 的归属。
 * 没有国别时与页头一样不画底板，只占位对齐。 */
function MachineMark({ country }: { country: string | null }) {
  if (!country) return <span className="rut-mark-slot" aria-hidden="true" />;
  return (
    <span className="nd-idplate">
      <span className="nd-idplate-clip">
        <RegionFlag code={country} square />
      </span>
    </span>
  );
}

/** 当前一栏：下行在上、上行在下，箭头与走势同色。连续 10 秒没有流量后只写空闲多久。
 * 读数未知写「—」；实时通道断开（整张卡或这一台机器）时停在最后一次读数，为 0 的也写「—」。 */
function CurrentRate({
  row,
  idle,
  idleMillis,
  frozen,
}: {
  row: CardRow;
  idle: boolean;
  idleMillis: number;
  frozen: boolean;
}) {
  if (row.down === null || row.up === null || (frozen && totalOf(row) === 0)) {
    return <span className="rut-cur none">—</span>;
  }
  if (idle) return <span className="rut-cur idle">{idleLabel(idleMillis)}</span>;
  const pair = Math.max(row.down, row.up);
  return (
    <span className="rut-cur">
      <span className="d">
        <Icon of="rx" size={11} className="rut-dir" />
        <Rate value={row.down} pair={pair} />
      </span>
      <span className="u">
        <Icon of="tx" size={11} className="rut-dir" />
        <Rate value={row.up} pair={pair} />
      </span>
    </span>
  );
}

interface RowMeta {
  /** 空闲多久；没有流量记录时为无穷大。 */
  idleMillis: number;
  idle: boolean;
  /** 用量之前的说明（接入点数、含转发）与之后的说明（历史与连接）。 */
  lead: string[];
  tail: string[];
}

/** 用户详情的一台入口机器：地区旗方牌、名称与近 1 小时用量、走势、当前下行 / 上行。 */
function TrafficItem({
  row,
  meta,
  frozen,
  peak,
  country,
}: {
  row: CardRow;
  meta: RowMeta;
  frozen: boolean;
  peak: number;
  country: string | null;
}) {
  const label = `${row.name}：${row.current === false || frozen ? '上次' : '当前'}下行 ${row.down === null ? '未知' : bytes(row.down) + '/s'}，上行 ${
    row.up === null ? '未知' : bytes(row.up) + '/s'
  }，近 1 小时 ${bytes(row.hourBytes)}`;
  const [figure, unit = ''] = bytes(row.hourBytes).split(' ');
  return (
    <div
      className={`rut-item${meta.idle ? ' idle' : ''}${row.offline ? ' offline' : ''}`}
      role="listitem"
      aria-label={label}
      data-key={row.key}
    >
      <MachineMark country={country} />
      <span className="rut-who">
        <b className="rut-name">{row.name}</b>
        <span
          className={`rut-meta${row.offline ? ' warn' : ''}`}
          title={[...meta.lead, bytes(row.hourBytes), ...meta.tail].join(' · ')}
        >
          {meta.lead.map(part => `${part} · `).join('')}
          <em>{figure}</em>
          {unit && ` ${unit}`}
          {meta.tail.map(part => ` · ${part}`).join('')}
        </span>
      </span>
      <HourSpark hour={row.hour} peak={peak} />
      <CurrentRate
        row={row}
        idle={meta.idle}
        idleMillis={meta.idleMillis}
        frozen={frozen || row.offline || row.current === false}
      />
    </div>
  );
}

function SourceName({ source }: { source: TrafficSource }) {
  return (
    <span className={`rut-source${source.muted ? ' muted' : ''}`}>
      {source.key === FORWARDING_SOURCE ? (
        <Icon of="forwarding" size={13} className="traffic-source-icon" />
      ) : (
        source.country && <RegionFlag code={source.country} />
      )}
      <span className="rut-source-name">{source.name}</span>
    </span>
  );
}

/** 当前来源按接入链合并，同一条链的多个接入点合为一项，已见过的零值来源仍保留。 */
function sourcesOf(row: CardRow, sourceOf: (key: string) => TrafficSource, counted: boolean) {
  const merged = new Map<string, TrafficSource & { value: number }>();
  for (const [key, rate] of Object.entries(row.sources ?? {})) {
    const value = counted ? rate.down + rate.up : 0;
    const source = sourceOf(key);
    const entry = merged.get(source.key) ?? { ...source, value: 0 };
    entry.value += value;
    merged.set(source.key, entry);
  }
  return [...merged.values()];
}

type TrafficTier = 'empty' | 'list' | 'crowd' | 'machines';

/** 机器详情 24 位以内全部列出；更多时折叠到首屏，默认按名称排，可继续展开。 */
function tierOf(page: 'node' | 'user', count: number): TrafficTier {
  if (count === 0) return 'empty';
  if (page === 'user') return 'machines';
  return count <= 24 ? 'list' : 'crowd';
}

/* ── 卡片 ── */

interface TrafficCardProps {
  page: 'node' | 'user';
  phase: TrafficPhase;
  partial: boolean;
  /** 实时通道断开时，最后一次有效数据的时刻。 */
  staleSince: number | null;
  now: number;
  live: ReadonlyMap<string, RetainedRow>;
  /** 最新报告完整时，不在报告里的已知对象当前为 0。 */
  complete: boolean;
  history: { model: HourModel | null; pending: boolean; failed: boolean };
  historyLabel?: string;
  historyNote?: string;
  nameOf: (key: string) => string;
  /** 用户详情：入口机器公网 IPv4 的归属地区，给身份牌的地区旗用。 */
  countryOf?: (key: string) => string | null;
  /** 用户详情：快照列出的全部入口机器。 */
  targets?: readonly string[];
  /** 机器用量：来源键（接入点 / 转发）对应的接入链名称与地区。 */
  sourceOf?: (key: string) => TrafficSource;
  className?: string;
}

function TrafficCard({
  page,
  phase,
  partial,
  staleSince,
  now,
  live,
  complete,
  history,
  historyLabel = '近 1 小时 · 每 30 秒',
  historyNote = '按真实采样窗口计算平均速率；确认重启停机期间为 0，未知缺失仍为空',
  nameOf,
  countryOf,
  targets = NO_TARGETS,
  sourceOf = fallbackSource,
  className = '',
}: TrafficCardProps) {
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState(0);
  const [narrow, setNarrow] = useState(false);
  const cardRef = useRef<HTMLElement>(null);
  const users = page === 'node';
  const unit = users ? '位' : '台';
  const stale = phase === 'offline' && staleSince !== null;
  const showsLive = phase === 'ready' || phase === 'recovering' || stale;

  const rows = useMemo(() => {
    const keys = new Set<string>([...live.keys(), ...targets]);
    history.model?.rows.forEach((_, key) => keys.add(key));
    return [...keys].map((key): CardRow => {
      const retained = live.get(key);
      const hour = history.model?.rows.get(key) ?? null;
      const absent = phase === 'ready' && complete ? 0 : null;
      const [tenant, user] = users ? key.split('/') : [undefined, undefined];
      return {
        key,
        name: users ? (retained?.name ?? hour?.user ?? user ?? key) : nameOf(key),
        detail: users ? (retained?.detail ?? hour?.tenant ?? tenant ?? '') : '',
        down: showsLive ? (retained?.down ?? absent) : null,
        up: showsLive ? (retained?.up ?? absent) : null,
        offline: !!retained?.offline,
        current: retained?.current ?? (phase === 'ready' && complete),
        measuredAt: retained?.measuredAt,
        ingresses: retained?.ingresses,
        forwardingDown: retained?.forwardingDown,
        forwardingUp: retained?.forwardingUp,
        sources: retained?.sources,
        hour,
        hourBytes: hour ? hour.hourDown + hour.hourUp : 0,
        lastActiveMs: Math.max(retained?.lastActiveMs ?? 0, hour?.lastActiveMs ?? 0) || null,
      };
    });
  }, [complete, users, history.model, live, nameOf, phase, showsLive, targets]);

  const tier = tierOf(page, rows.length);
  const [sort, setSort] = useState<TrafficSort>('name');
  const order = useListOrder(rows, sort, now);
  const byKey = new Map(rows.map(row => [row.key, row]));
  const ordered = order.keys.map(key => byKey.get(key)).filter((row): row is CardRow => !!row);
  const needle = query.trim().toLowerCase();
  const matched = needle ? ordered.filter(row => row.name.toLowerCase().includes(needle)) : ordered;
  const searchable = !users && (rows.length > 6 || query !== '');
  const fold = narrow ? CROWD_FOLD_NARROW : CROWD_FOLD;
  const limit = tier === 'crowd' ? (needle ? MORE_STEP : fold) + expanded : Number.POSITIVE_INFINITY;
  const visible = matched.slice(0, limit);
  const latest = [...rows]
    .sort(compareRows(sort))
    .filter(row => !needle || row.name.toLowerCase().includes(needle))
    .slice(0, limit);
  const rankChanged =
    sort !== 'name' && visible.map(row => row.key).join('\n') !== latest.map(row => row.key).join('\n');
  const listRef = useFlip(visible.map(row => row.key).join('\n'));

  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card) return;
    const update = () => setNarrow(card.clientWidth > 0 && card.clientWidth <= 700);
    update();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(update);
    observer.observe(card);
    return () => observer.disconnect();
  }, []);

  /* 合计只算当前有效的读数：完整报告里缺席的已知行算 0，恢复中的旧读数超过新鲜期不再计入。 */
  const counted = (row: CardRow) =>
    !(phase === 'ready' && row.current === false) &&
    !(phase === 'recovering' && row.measuredAt !== undefined && now - row.measuredAt > FRESH_MILLIS);
  const totals = rows.reduce(
    (sum, row) => {
      if (!counted(row)) return sum;
      sum.down += row.down ?? 0;
      sum.up += row.up ?? 0;
      if (totalOf(row) > 0) sum.active += 1;
      return sum;
    },
    { down: 0, up: 0, active: 0 },
  );
  /* 当前经过的接入链：各用户按来源分的当前速率映射到接入链后相加，与上面的合计同一口径。 */
  const chainRates = users
    ? [
        ...rows
          .flatMap(row => sourcesOf(row, sourceOf, showsLive && row.down !== null && counted(row)))
          .reduce((merged, source) => {
            const entry = merged.get(source.key) ?? { ...source, value: 0 };
            entry.value += source.value;
            return merged.set(source.key, entry);
          }, new Map<string, TrafficSource & { value: number }>())
          .values(),
      ].sort((a, b) => nameOrder.compare(a.name, b.name) || a.key.localeCompare(b.key))
    : [];
  const currentSum = totals.down + totals.up;
  const hero = observeBpsReadingParts(currentSum * 8);
  const pair = Math.max(totals.down, totals.up);
  const peak = history.model?.peak ?? 0;

  const chart = useMemo(() => {
    const model = history.model;
    if (!model) return null;
    const toBits = (value: number | null) => (value === null ? null : (value * 8) / SLOT_SECS);
    const rx = model.totalDown.map(toBits);
    const tx = model.totalUp.map(toBits);
    return {
      rx,
      tx,
      unit: throughputAxis(rx, tx).unit,
      total: model.recordedBytes ?? [...model.rows.values()].reduce((sum, row) => sum + row.hourDown + row.hourUp, 0),
    };
  }, [history.model]);

  const lamp =
    phase === 'ready' && !partial
      ? ''
      : phase === 'ready' || phase === 'pending' || phase === 'recovering'
        ? 'idle'
        : 'warn';
  const status = phase === 'ready' && partial ? PARTIAL_TEXT : PHASE_TEXT[phase];
  const emptyText = needle && matched.length === 0 ? `没有匹配「${query.trim()}」的${users ? '用户' : '机器'}` : null;
  const rest = Math.max(0, matched.length - visible.length);

  const metaOf = (row: CardRow): RowMeta => {
    const idleMillis = row.lastActiveMs === null ? Number.POSITIVE_INFINITY : now - row.lastActiveMs;
    // 第二行只放身份与用量：接入点数（用户详情）· 含转发 · 近 1 小时用量 · 历史与连接说明；
    // 空闲多久写在「当前」一栏。机器详情不再写租户。
    return {
      idleMillis,
      idle:
        phase === 'ready' && row.current !== false && !row.offline && totalOf(row) === 0 && idleMillis >= IDLE_MILLIS,
      lead: [
        !users && row.ingresses ? `${row.ingresses} 个接入点` : '',
        row.forwardingDown !== undefined || row.hour?.forwarding ? '含转发' : '',
      ].filter(Boolean),
      tail: [
        row.hour?.unplaced ? '部分历史无精确时间' : '',
        row.offline ? '未连接' : row.current === false && row.down !== null ? '上次读数' : '',
        !users && row.measuredAt !== undefined && now - row.measuredAt >= 3_000
          ? `${Math.floor((now - row.measuredAt) / 1_000)} 秒前收到`
          : '',
      ].filter(Boolean),
    };
  };

  const nowPanel = (
    <section className="rut-now" aria-label="当前速率">
      <div className="user-usage-hero">
        <span className="user-usage-hero-label" title={users ? undefined : RATE_SUM_NOTE}>
          {phase === 'recovering' || stale ? '上次速率合计' : partial ? '部分速率合计' : '最近速率合计'}
        </span>
        <strong className="user-usage-hero-value">
          {showsLive ? (
            <>
              {currentSum > 0 ? hero.number : '0'} <small>{hero.unit}</small>
            </>
          ) : (
            <span className="rut-wait">—</span>
          )}
        </strong>
        <span className="rut-hero-sub">
          {showsLive ? (
            <>
              <b>{totals.active}</b> {unit}活跃 ·{' '}
              {!users && phase === 'ready' ? (
                `${rows.filter(row => row.current).length} / ${targets.length} 台有读数`
              ) : (
                <>
                  共 <b>{rows.length}</b> {unit}
                </>
              )}
            </>
          ) : (
            `共 ${rows.length} ${unit}`
          )}
        </span>
      </div>
      <div>
        <div className="usage-split" aria-hidden="true">
          {totals.down > 0 && <i className="down" style={{ flexGrow: totals.down }} />}
          {totals.up > 0 && <i className="up" style={{ flexGrow: totals.up }} />}
        </div>
        <dl className="usage-io">
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{showsLive ? rateText(totals.down, pair) : '—'}</dd>
            <dd className="usage-share">{showsLive && currentSum > 0 ? shareOf(totals.down, currentSum) : ''}</dd>
          </div>
          <div className="up">
            <dt>
              <i aria-hidden="true" />
              上行
            </dt>
            <dd>{showsLive ? rateText(totals.up, pair) : '—'}</dd>
            <dd className="usage-share">{showsLive && currentSum > 0 ? shareOf(totals.up, currentSum) : ''}</dd>
          </div>
        </dl>
      </div>
      {users && (
        <section className="rut-chains" aria-label="当前经过的接入链">
          <span className="rut-chains-title">当前经过的接入链</span>
          {chainRates.length === 0 ? (
            <span className="rut-chains-none">{showsLive ? '没有正在传输的连接' : '—'}</span>
          ) : (
            <ol>
              {chainRates.map(chain => (
                <li
                  key={chain.key}
                  title={
                    showsLive && (chain.value > 0 || complete)
                      ? `${chain.name} · 占当前 ${shareOf(chain.value, currentSum)}`
                      : chain.name
                  }
                >
                  <SourceName source={chain} />
                  <b>
                    <Rate value={showsLive && (chain.value > 0 || complete) ? chain.value : null} pair={chain.value} />
                  </b>
                </li>
              ))}
            </ol>
          )}
        </section>
      )}
    </section>
  );

  const searchControl = searchable && (
    <span className="user-list-search">
      <Icon of="search" size={13} className="user-list-search-icon" />
      <input
        type="search"
        value={query}
        aria-label="搜索机器"
        placeholder="搜索机器"
        autoComplete="off"
        spellCheck={false}
        onChange={event => setQuery(event.target.value)}
        onKeyDown={event => {
          if (event.key === 'Escape') setQuery('');
        }}
      />
      {query && (
        <button type="button" aria-label="清空搜索" title="清空" onClick={() => setQuery('')}>
          <Icon of="close" size={13} />
        </button>
      )}
    </span>
  );

  const staleNotice = stale && (
    <p className="rut-notice" role="status">
      <i className="node-lamp warn" />
      实时通道未连接：当前速率停在 {clockText(staleSince)}，{users ? '所选区间历史' : '近 1 小时'}仍每 30 秒更新
    </p>
  );

  const main = !users && (
    <div className="rut-main">
      <div className="rut-tools">
        {searchControl}
        {needle && (
          <span className="rut-count">
            {matched.length} / {ordered.length}
          </span>
        )}
        {rankChanged && (
          <span className="rut-rank-change" role="status" aria-live="polite" aria-atomic="true">
            <span>排名已变化</span>
            <button className="btn sm" type="button" aria-label="按最新排名重排" onClick={order.resort}>
              重排
            </button>
          </span>
        )}

        <SegmentedControl
          className="rut-sort"
          ariaLabel="排序"
          value={sort}
          onChange={value => {
            setSort(value);
            setExpanded(0);
          }}
          options={USER_SORTS}
        />
      </div>
      {staleNotice}
      <p className="rut-caption" aria-hidden="true">
        <span>走势：近 1 小时 · 每 30 秒</span>
        <span className="rut-legend">
          <i className="d" />
          下行
          <i className="u" />
          上行
        </span>
        <span>用量：近 1 小时</span>
      </p>
      <div ref={listRef} className="rut-grid machines" role="list" aria-label="各入口机器流量">
        {visible.map(row => (
          <TrafficItem
            key={row.key}
            row={row}
            meta={metaOf(row)}
            frozen={stale}
            peak={peak}
            country={countryOf?.(row.key) ?? null}
          />
        ))}
      </div>
      {emptyText && <p className="rut-empty">{emptyText}</p>}
      {rest > 0 && (
        <div className="rut-more">
          <button
            className="btn text sm"
            type="button"
            onClick={() => setExpanded(value => value + MORE_STEP)}
            aria-expanded={expanded > 0}
          >
            显示更多 {Math.min(MORE_STEP, rest)} {unit}
          </button>
          {expanded > 0 && (
            <button className="btn text sm" type="button" onClick={() => setExpanded(0)}>
              收起
            </button>
          )}
        </div>
      )}
      {rest === 0 && expanded > 0 && (
        <button className="btn text sm rut-more" type="button" onClick={() => setExpanded(0)}>
          收起
        </button>
      )}
    </div>
  );

  const hourPanel = (
    <section className="rut-hour" aria-label={users ? '所选区间历史流量' : '近 1 小时'}>
      <div className="rut-hour-head">
        <span title={historyNote}>{historyLabel}</span>
        {chart && <span className="chart-unit">({chart.unit.name})</span>}
        <span className="rut-legend" aria-hidden="true">
          <i className="d" />
          下行
          <i className="u" />
          上行
        </span>
        {chart && (
          <b title={users ? '所选区间保留的记账窗口总量' : undefined}>
            <Bytes value={chart.total} />
          </b>
        )}
      </div>
      {history.model && chart && history.model.totalDown.some(value => value !== null) ? (
        <Suspense fallback={<div className="rut-chart-wait" />}>
          <LazyThroughputChart
            presentation={users ? 'bar' : 'line'}
            timesUnixSecs={history.model.slots}
            rangeStartUnixSecs={history.model.rangeStart}
            rangeEndUnixSecs={history.model.rangeEnd}
            rx={chart.rx}
            tx={chart.tx}
            rxName="下行"
            txName="上行"
          />
        </Suspense>
      ) : (
        <div className="rut-chart-wait">
          {history.failed ? (
            <span className="rut-chart-note">{users ? '所选区间' : '近 1 小时'}数据暂不可用</span>
          ) : history.model ? (
            <span className="rut-chart-note">{users ? '所选区间' : '近 1 小时'}没有保留的上报记录</span>
          ) : null}
        </div>
      )}
    </section>
  );

  const emptyCopy = history.pending ? '正在读取近 1 小时流量' : '尚未授权入口机器';

  if (users) {
    return (
      <section
        ref={cardRef}
        className={`panel config-panel realtime-user-traffic rut-node${stale ? ' stale' : ''}${className ? ` ${className}` : ''}`}
        aria-label="实时流量"
      >
        <header className="realtime-panel-header">
          <span title={SCOPE_NOTE}>
            <PanelTitle of="usage">实时流量</PanelTitle>
          </span>
          <span className={`rut-status${lamp === 'warn' ? ' warn' : ''}`} role="status">
            <i className={`node-lamp${lamp ? ` ${lamp}` : ''}`} />
            {status}
          </span>
        </header>
        <div className="rut-body panel-full-bleed-body">
          {staleNotice}
          <div className="rut-overview">
            {nowPanel}
            {hourPanel}
          </div>
        </div>
      </section>
    );
  }

  return (
    <section
      ref={cardRef}
      className={`panel config-panel realtime-user-traffic${stale ? ' stale' : ''} ${className}`}
      aria-label="实时流量"
    >
      <header className="realtime-panel-header">
        <span title={SCOPE_NOTE}>
          <PanelTitle of="usage">实时流量</PanelTitle>
        </span>
        <span className={`rut-status${lamp === 'warn' ? ' warn' : ''}`} role="status">
          <i className={`node-lamp${lamp ? ` ${lamp}` : ''}`} />
          {status}
        </span>
      </header>
      <div className="rut-body panel-full-bleed-body">
        {tier === 'empty' ? (
          phase === 'ready' || phase === 'empty' ? (
            <p className="rut-empty rut-zero">{emptyCopy}</p>
          ) : (
            <div className="rut-split rut-zero-state">
              {nowPanel}
              <p className="rut-empty rut-zero">{emptyCopy}</p>
            </div>
          )
        ) : (
          <>
            <div className="rut-cockpit">
              {nowPanel}
              {hourPanel}
            </div>
            {main}
          </>
        )}
      </div>
    </section>
  );
}

const clockText = (millis: number | null) =>
  millis === null ? '—' : new Date(millis).toLocaleTimeString('zh-CN', { hour12: false });

function useHourHistory(queryKey: readonly unknown[], load: () => Promise<UserTrafficHistory>) {
  const history = useMetricQuery({
    queryKey,
    queryFn: load,
    refetchInterval: HISTORY_REFRESH_MILLIS,
    staleTime: HISTORY_REFRESH_MILLIS / 2,
  });
  const model = useMemo(() => (history.data ? hourModel(history.data) : null), [history.data]);
  return { model, pending: history.isPending, failed: history.isError && !history.data };
}

/* ── 机器用量 ── */

/** 用量弹层的机器实时流量。`sourceOf` 把接入点映射到接入链（来自页面已有的模型快照）。 */
export function NodeUserTrafficCard({
  nodeId,
  sourceOf,
  targets = NO_TARGETS,
  period = 'day',
  range,
}: {
  nodeId: string;
  sourceOf?: (key: string) => TrafficSource;
  targets?: readonly string[];
  period?: SiteUsagePeriod;
  range?: TrafficTimeRange;
}) {
  return (
    <NodeTrafficCard key={nodeId} nodeId={nodeId} sourceOf={sourceOf} targets={targets} period={period} range={range} />
  );
}

function NodeTrafficCard({
  nodeId,
  sourceOf,
  targets,
  period,
  range,
}: {
  nodeId: string;
  sourceOf?: (key: string) => TrafficSource;
  targets: readonly string[];
  period: SiteUsagePeriod;
  range?: TrafficTimeRange;
}) {
  const { last, now, connected, enabled, observingSince } = useNodeRealtime(nodeId);
  const report = last?.event.sample.user_traffic;
  const fresh =
    !!last && now - last.arrived <= FRESH_MILLIS && now - last.event.received_at_unix_millis <= FRESH_MILLIS;
  let phase: TrafficPhase = 'pending';
  if (enabled === false) phase = 'disabled';
  else if (!connected && (last || now - observingSince > 5000)) phase = 'offline';
  else if (last && !fresh) phase = 'offline';
  else if (fresh && !report) phase = 'unsupported';
  else if (report?.state === 'unavailable') phase = 'unavailable';
  else if (report?.forwarding_expected && !report.forwarding_supported) phase = 'forwarding-unsupported';
  else if (
    fresh &&
    report?.state === 'ready' &&
    report.all_users &&
    (!(report.has_gap || report.truncated) || (report.users?.length ?? 0) > 0)
  )
    phase = 'ready';
  const ready = phase === 'ready' && !!report;
  const complete = ready && !report.has_gap && !report.truncated;
  const liveRows = useMemo(() => (ready ? aggregateUserTraffic(report?.users ?? []) : []), [ready, report]);
  const at = ready ? (last?.event.received_at_unix_millis ?? null) : null;
  const retained = useRetained(at, liveRows, complete, last?.event);
  if (phase === 'pending' && report?.state === 'warming' && retained.at !== null && now - retained.at <= FRESH_MILLIS)
    phase = 'recovering';
  const query = useMetricQuery({
    queryKey: ['node-traffic-history', nodeId, period, range ?? null],
    queryFn: () => fetchNodeTrafficHistory(nodeId, period, range),
    refetchInterval: HISTORY_REFRESH_MILLIS,
    staleTime: HISTORY_REFRESH_MILLIS / 2,
  });
  const model = useMemo(() => (query.data ? nodeHistoryModel(query.data) : null), [query.data]);
  const history = { model, pending: query.isPending, failed: query.isError && !query.data };
  const periodLabel = range ? '自选区间' : { day: '当天', week: '当周', month: '当月' }[period];
  const step = query.data?.bucket_secs;
  const stepLabel =
    step === undefined
      ? ''
      : step < 60
        ? `每 ${step} 秒`
        : step < 3600
          ? `每 ${step / 60} 分钟`
          : `每 ${step / 3600} 小时`;
  const nameOf = useMemo(() => (key: string) => key, []);
  return (
    <TrafficCard
      page="node"
      phase={phase}
      partial={!!(report?.has_gap || report?.truncated)}
      staleSince={phase === 'offline' && retained.rows.size > 0 ? retained.at : null}
      now={now}
      live={retained.rows}
      complete={complete}
      history={history}
      historyLabel={`${periodLabel}${stepLabel ? ` · ${stepLabel}` : ''} · 已观测窗口均值`}
      historyNote={`仅汇总保留的真实记账窗口；未采集或已清理的时段为空${query.data?.has_gap ? '；部分区间采样不完整' : ''}${query.data?.has_unplaced ? '；无时间边界的累计量未绘入图表' : ''}`}
      nameOf={nameOf}
      sourceOf={sourceOf}
      targets={targets}
    />
  );
}

/** 共用图表模型仍以每 30 秒等效字节表示速率，累计量直接采用服务端窗口总量。 */
export function nodeHistoryModel(history: NodeTrafficHistory): HourModel {
  const totalDown = history.points.map(point => (point[2] === null ? null : point[2] * SLOT_SECS));
  const totalUp = history.points.map(point => (point[1] === null ? null : point[1] * SLOT_SECS));
  return {
    rangeStart: history.range_start_unix_secs,
    rangeEnd: history.range_end_unix_secs,
    slots: history.points.map(point => point[0]),
    totalDown,
    totalUp,
    rows: new Map(),
    peak: Math.max(0, ...history.points.flatMap(point => [point[1] ?? 0, point[2] ?? 0])),
    recordedBytes: history.recorded_bytes,
  };
}

/* ── 用户详情 ── */

/** 用户详情的一台入口机器：当前速率、是否在线，以及这一份数据能否代表全部授权。 */
function userNodeRow(node: UserTrafficNode, tenantId: string, userId: string, now: number) {
  const event = node.samples.at(-1);
  const report = event?.sample.user_traffic;
  const base = { key: node.node_id, name: node.node_id, detail: '', current: false };
  if (!node.connected || !event || now - event.received_at_unix_millis > FRESH_MILLIS) {
    return { row: { ...base, down: null, up: null, offline: true }, state: 'offline' as const, at: null };
  }
  if (!report) return { row: { ...base, down: null, up: null }, state: 'unsupported' as const, at: null };
  if (report.forwarding_expected && !report.forwarding_supported)
    return { row: { ...base, down: null, up: null }, state: 'forwarding-unsupported' as const, at: null };
  if (report.state === 'unavailable') {
    return { row: { ...base, down: null, up: null }, state: 'unavailable' as const, at: null };
  }
  if (report.state !== 'ready') return { row: { ...base, down: null, up: null }, state: 'pending' as const, at: null };
  const users = (report.users ?? []).filter(user => user.tenant_id === tenantId && user.user_id === userId);
  if ((report.has_gap || report.truncated) && users.length === 0) {
    return { row: { ...base, down: null, up: null }, state: 'missing' as const, at: null };
  }
  const forwarding = users.filter(user => user.forwarding_rule_id !== undefined);
  const hasForwarding = report.forwarding_expected || forwarding.length > 0;
  return {
    row: {
      ...base,
      current: true,
      measuredAt: event.received_at_unix_millis,
      down: users.reduce((sum, user) => sum + user.downlink_bytes_per_sec, 0),
      up: users.reduce((sum, user) => sum + user.uplink_bytes_per_sec, 0),
      ingresses: new Set(users.map(user => user.ingress_id).filter(Boolean)).size,
      forwardingDown: hasForwarding
        ? forwarding.reduce((sum, user) => sum + user.downlink_bytes_per_sec, 0)
        : undefined,
      forwardingUp: hasForwarding ? forwarding.reduce((sum, user) => sum + user.uplink_bytes_per_sec, 0) : undefined,
    },
    state: (report.has_gap || report.truncated ? 'partial' : 'ready') as 'partial' | 'ready',
    at: event.received_at_unix_millis,
  };
}

interface UserTrafficCardProps {
  tenantId: string;
  userId: string;
  nameOf: (id: string) => string;
}

export function UserRealtimeTrafficCard(props: UserTrafficCardProps) {
  return <UserTrafficCard key={`${props.tenantId}/${props.userId}`} {...props} />;
}

function UserTrafficCard({ tenantId, userId, nameOf }: UserTrafficCardProps) {
  const { payload, now, failed } = useUserTrafficSource(tenantId, userId);
  const nodes = useMemo(
    () => (payload?.nodes ?? []).map(node => userNodeRow(node, tenantId, userId, now)),
    [now, payload, tenantId, userId],
  );
  const ready = nodes.filter(node => node.state === 'ready' || node.state === 'partial');
  let phase: TrafficPhase = 'pending';
  if (!payload && failed) phase = 'unavailable';
  else if (payload && !payload.policy.enabled) phase = 'disabled';
  else if (payload?.nodes.length === 0) phase = 'empty';
  else if (ready.length > 0) phase = 'ready';
  else if (nodes.some(node => node.state === 'forwarding-unsupported')) phase = 'forwarding-unsupported';
  else if (nodes.some(node => node.state === 'unsupported')) phase = 'unsupported';
  else if (nodes.some(node => node.state === 'unavailable')) phase = 'unavailable';
  else if (payload && nodes.every(node => node.state === 'offline')) phase = 'offline';
  const liveRows = useMemo(() => nodes.map(node => node.row), [nodes]);
  const at = ready.length > 0 ? Math.max(...ready.map(node => node.at ?? 0)) : null;
  const retained = useRetained(at, liveRows, false, payload);
  // Renewing a subscription can briefly warm the Agent's counters again. Keep the last
  // measured rates within their original freshness budget; warming frames never renew it.
  if (
    phase === 'pending' &&
    nodes.some(node => node.state === 'pending') &&
    retained.at !== null &&
    now - retained.at <= FRESH_MILLIS
  )
    phase = 'recovering';
  // 未连接的机器只标记离线，读数保留最后一次有效数据
  const live = useMemo(() => {
    const next = new Map(retained.rows);
    for (const node of nodes) {
      const row = next.get(node.row.key);
      if (row) next.set(node.row.key, { ...row, offline: node.state === 'offline', current: node.row.current });
    }
    return next;
  }, [nodes, retained]);
  const history = useHourHistory(['user-traffic-history', 'user', tenantId, userId], () =>
    fetchUserTrafficHistory(tenantId, userId),
  );
  const targets = useMemo(() => (payload?.nodes ?? []).map(node => node.node_id), [payload]);
  const countryOf = useNodeCountries();
  return (
    <TrafficCard
      page="user"
      phase={phase}
      partial={nodes.some(node => node.state !== 'ready')}
      staleSince={phase === 'offline' && retained.rows.size > 0 ? retained.at : null}
      now={now}
      live={live}
      complete={false}
      history={history}
      nameOf={nameOf}
      countryOf={countryOf}
      targets={targets}
      className="user-dcard"
    />
  );
}
