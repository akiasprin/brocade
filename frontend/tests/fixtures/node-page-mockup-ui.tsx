// Option pages for the machine page redesign mockup (机器页). Never imported by the app.
//
// Every option keeps the production list title bar (ListIcon, 机器, 多选, ＋ 纳管机器) and the production
// queries, ordering and status judgements; only the body below the title bar differs. D additionally renders
// the production machine detail beside its roster, through the exported NodesPane.
import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchLatestNodePingProbes,
  fetchNodeNicListWindows,
  fetchNodeTraffic,
  fetchNodes,
  type NodeAgentStateItem,
  type NodeNicView,
  type NodePingProbeLatestView,
  type NodeTrafficItem,
  type PingProbePoint,
} from '../../src/api';
import { navigate, navigateInPlace } from '../../src/forge/route';
import {
  NodesPane,
  nodeLampState,
  nodeListSnapshotFresh,
  nodeRemovalReady,
  prefetchNodeDetailData,
  type Drill,
} from '../../src/panes/nodes';
import { can, isVisitor, useSession } from '../../src/session';
import { EmptyState } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { Icon, ListIcon } from '../../src/ui/icons';
import { useMetricQuery } from '../../src/ui/metric-cadence';
import { observeBpsReadingParts } from '../../src/ui/observe-chart';
import { pingLatencyMs, pingSampleLost } from '../../src/ui/ping-probe';
import { RegionFlag } from '../../src/ui/region-flag';
import { sparklinePaths } from '../../src/ui/sparkline';
import { useNarrow } from '../../src/ui/viewport';
import type { Win } from '../../src/wm/store';

export type Option = 'a' | 'b' | 'c' | 'd';

/* ══ 与生产列表相同的取数、排序与判定 ══ */

const LIST_NIC_WINDOWS = 24;
/** 走势的最低尺度与生产机器卡相同（1 Mbit/s），心跳流量不会被放大成满幅波峰。 */
const MIN_SCALE_BPS = 1_000_000;
const NAME_ORDER = new Intl.Collator('zh', { numeric: true, sensitivity: 'base' });
const nodeLabel = (node: NodeAgentStateItem) => node.name || node.node_id;

type Tone = 'ok' | 'warn' | 'bad' | 'idle';

interface Reading {
  number: string;
  unit: string;
}

interface Probe {
  name: string;
  ms: number | null;
  lost: boolean;
}

interface Entry {
  node: NodeAgentStateItem;
  /** 已进入退役流程（退役中、已退役、待清理、强制退役）。 */
  retired: boolean;
  tone: Tone;
  /** 生产状态灯的完整说明，放在 title 里。 */
  why: string;
  /** 可见的短状态：正常为 null。 */
  status: string | null;
  rate: { total: number; rx: number; tx: number } | null;
  /** 近 12 分钟每个 30 秒窗口的网卡收发合计（bit/s），缺口为 null。 */
  series: (number | null)[];
  month: { used: number; quota: number | null } | null;
  /** null 表示没有配置 TCP 探测目标，此时用地址代替延迟。 */
  probes: Probe[] | null;
  probesStale: boolean;
  removable: boolean;
}

function pollTone(node: NodeAgentStateItem): 'ok' | 'bad' | 'idle' {
  if (node.runtime_report_fresh) return 'ok';
  if (!node.last_poll_at) return 'idle';
  const at = parseTime(node.last_poll_at);
  if (Number.isNaN(at)) return 'idle';
  return Date.now() - at > 60_000 ? 'bad' : 'ok';
}

const parseTime = (raw: string) => Date.parse(raw.endsWith('Z') || raw.includes('+') ? raw : `${raw}Z`);

function lifecycleLabel(node: NodeAgentStateItem): string | null {
  switch (node.lifecycle_phase) {
    case 'active':
      return null;
    case 'retiring':
      return '退役中';
    case 'retired':
      return node.lifecycle_last_error ? '待清理' : '已退役';
    case 'abandoned':
      return '强制退役';
  }
}

function durationText(seconds: number): string {
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} 小时`;
  return `${Math.floor(seconds / 86_400)} 天`;
}

function probesOf(view: NodePingProbeLatestView | undefined, intervalSecs: number, nowMs: number) {
  const tcp = (view?.targets ?? []).filter(target => target.kind === 'tcp');
  if (tcp.length === 0) return { probes: null, stale: false };
  const staleAfter = Math.max(intervalSecs * 2, 30);
  let stale = false;
  const probes = tcp.map(target => {
    const sample: PingProbePoint | null = (target.ipv4 ?? target.ipv6)?.latest ?? null;
    if (sample && nowMs / 1000 - sample.probed_at_unix_secs > staleAfter) stale = true;
    return {
      name: target.name,
      ms: sample && sample.attempted ? pingLatencyMs(sample) : null,
      lost: pingSampleLost(sample ?? undefined),
    };
  });
  return { probes, stale };
}

function monthOf(traffic: NodeTrafficItem | undefined): Entry['month'] {
  if (!traffic) return null;
  if (traffic.tracking_started_at_unix_secs === null && traffic.calibrated_at_unix_secs === null) return null;
  return { used: Number(traffic.total_bytes), quota: traffic.quota_bytes ? Number(traffic.quota_bytes) : null };
}

function useFleet() {
  const qc = useQueryClient();
  const { who } = useSession();
  const now = useNow();
  const nodes = useQuery({
    queryKey: ['nodes'],
    queryFn: () => fetchNodes(),
    refetchInterval: 10_000,
    refetchOnMount: 'always',
  });
  const traffic = useMetricQuery({
    queryKey: ['node-traffic'],
    queryFn: fetchNodeTraffic,
    enabled: nodes.isSuccess,
    refetchInterval: 30_000,
    retry: false,
  });
  const load = useMetricQuery({
    queryKey: ['node-nic-list', 'windows', LIST_NIC_WINDOWS],
    queryFn: () => fetchNodeNicListWindows(LIST_NIC_WINDOWS),
    enabled: nodes.isSuccess,
    refetchInterval: 30_000,
    retry: false,
  });
  const ping = useMetricQuery({
    queryKey: ['ping-probe-nodes', 'latest'],
    queryFn: () => fetchLatestNodePingProbes(),
    enabled: nodes.isSuccess,
    refetchInterval: query => Math.max(query.state.data?.interval_secs ?? 10, 10) * 1_000,
    retry: false,
  });
  const fresh = nodeListSnapshotFresh(nodes.dataUpdatedAt, now);
  // 读数随时钟每秒重算：失联时长、探测是否过期都与当前时间有关。
  const entries = useMemo<Entry[]>(() => {
    const trafficOf = new Map((traffic.data?.nodes ?? []).map(item => [item.node_id, item]));
    const loadOf = new Map<string, NodeNicView>((load.data?.nodes ?? []).map(item => [item.node_id, item]));
    const pingOf = new Map((ping.data?.nodes ?? []).map(item => [item.node_id, item]));
    const interval = ping.data?.interval_secs ?? 60;
    const list = [...(nodes.data?.nodes ?? [])].sort(
      (a, b) =>
        Number(!!a.retired_at) - Number(!!b.retired_at) ||
        NAME_ORDER.compare(nodeLabel(a), nodeLabel(b)) ||
        NAME_ORDER.compare(a.node_id, b.node_id),
    );
    const isolated = new Set(list.filter(node => node.operationally_isolated).map(node => node.node_id));
    return list.map(node => {
      const retired = node.lifecycle_phase !== 'active';
      const live = fresh ? pollTone(node) : 'idle';
      const lamp = fresh
        ? nodeLampState(node, undefined, isolated)
        : ({ tone: 'idle', why: '缓存状态待确认' } as { tone: Tone; why: string });
      const status =
        lifecycleLabel(node) ??
        (lamp.tone === 'ok'
          ? null
          : live === 'bad' && node.last_poll_at
            ? `失联 ${durationText((now - parseTime(node.last_poll_at)) / 1000)}`
            : lamp.why);
      const samples = (loadOf.get(node.node_id)?.series ?? []).slice(-LIST_NIC_WINDOWS);
      const last = samples.at(-1);
      const { probes, stale } = probesOf(pingOf.get(node.node_id), interval, now);
      return {
        node,
        retired,
        tone: lamp.tone,
        why: lamp.why,
        status,
        rate:
          last && !last.has_gap && live === 'ok' && !retired
            ? { total: last.nic_rx_bps + last.nic_tx_bps, rx: last.nic_rx_bps, tx: last.nic_tx_bps }
            : null,
        series: samples.map(sample => (sample.has_gap ? null : sample.nic_rx_bps + sample.nic_tx_bps)),
        month: monthOf(trafficOf.get(node.node_id)),
        probes: retired ? null : probes,
        probesStale: stale || live === 'bad',
        removable: nodeRemovalReady(node),
      };
    });
  }, [nodes.data, traffic.data, load.data, ping.data, fresh, now]);
  const targets = useMemo(() => {
    const names: string[] = [];
    for (const view of ping.data?.nodes ?? []) {
      for (const target of view.targets) {
        if (target.kind === 'tcp' && !names.includes(target.name)) names.push(target.name);
      }
    }
    return names;
  }, [ping.data]);
  return {
    nodes,
    entries,
    targets,
    prepare: (id: string) => void prefetchNodeDetailData(qc, id, !isVisitor(who)),
  };
}
type Fleet = ReturnType<typeof useFleet>;

/* ══ 多选：与生产相同的一个开关两类操作 ══ */

function useSelection(entries: Entry[]) {
  const [selecting, setSelecting] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const exit = () => {
    setSelecting(false);
    setPicked(new Set());
  };
  return {
    selecting,
    picked,
    begin: () => {
      setSelecting(true);
      setPicked(new Set());
    },
    exit,
    toggle: (id: string) =>
      setPicked(previous => {
        const next = new Set(previous);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      }),
    selectable: (entry: Entry) => !entry.node.retired_at || entry.removable,
    pickedLive: entries.filter(entry => picked.has(entry.node.node_id) && !entry.node.retired_at).length,
    pickedRetired: entries.filter(entry => picked.has(entry.node.node_id) && entry.removable).length,
  };
}
type Selection = ReturnType<typeof useSelection>;

/** 生产 NodeList 的标题栏原样：图标、机器、多选与纳管。 */
function TitleBar({ fleet, selection }: { fleet: Fleet; selection: Selection }) {
  const { who } = useSession();
  const system = can(who.role, 'system');
  const removable = fleet.entries.some(entry => entry.removable);
  return (
    <header>
      <ListIcon of="nodes" />
      <h4>机器</h4>
      <span className="sp" />
      {!isVisitor(who) &&
        (selection.selecting ? (
          <>
            <button className="btn" type="button" onClick={selection.exit}>
              取消
            </button>
            <button className="btn danger" type="button" disabled={selection.pickedLive === 0} onClick={selection.exit}>
              {`退役下线${selection.pickedLive > 0 ? ` ${selection.pickedLive}` : ''}`}
            </button>
            {removable && (
              <button
                className="btn danger node-remove-button"
                type="button"
                disabled={selection.pickedRetired === 0}
                onClick={selection.exit}
              >
                <Icon of="trash" size={13} className="node-remove-button-icon" />
                {`移除选中${selection.pickedRetired > 0 ? ` ${selection.pickedRetired}` : ''}`}
              </button>
            )}
          </>
        ) : (
          <>
            {fleet.entries.length > 0 && (
              <button className="btn" type="button" disabled={!system} onClick={selection.begin}>
                多选
              </button>
            )}
            <button
              className="btn primary"
              type="button"
              disabled={!system}
              onClick={() => navigate('nodes', { p: 'provision', step: 1 })}
            >
              ＋ 纳管机器
            </button>
          </>
        ))}
    </header>
  );
}

function EmptyFleet() {
  const { who } = useSession();
  return (
    <EmptyState
      icon="nodes"
      title="还没有机器"
      action={
        <button
          className="btn primary"
          disabled={!can(who.role, 'system')}
          onClick={() => navigate('nodes', { p: 'provision', step: 1 })}
        >
          纳管第一台机器
        </button>
      }
    >
      先登记机器身份与网络信息，再在目标机器上安装 Agent。
    </EmptyState>
  );
}

/* ══ 共用部件 ══ */

/** 整块可点：多选时勾选，否则进入详情；悬停 120ms 后预取详情数据。 */
function useOpen(fleet: Fleet, selection: Selection, entry: Entry, onOpen?: (id: string) => void) {
  const id = entry.node.node_id;
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const open = () => {
    if (selection.selecting) {
      if (selection.selectable(entry)) selection.toggle(id);
      return;
    }
    window.clearTimeout(timer.current);
    fleet.prepare(id);
    if (onOpen) onOpen(id);
    else navigate('nodes', { p: 'node', id });
  };
  return {
    role: 'button' as const,
    tabIndex: 0,
    'data-route-focus': `node:${id}`,
    onClick: open,
    onKeyDown: (event: ReactKeyboardEvent) => {
      if (event.target !== event.currentTarget) return;
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        open();
      }
    },
    onMouseEnter: () => {
      if (selection.selecting || timer.current) return;
      timer.current = window.setTimeout(() => fleet.prepare(id), 120);
    },
    onMouseLeave: () => {
      window.clearTimeout(timer.current);
      timer.current = 0;
    },
  };
}

function PickBox({ entry, selection }: { entry: Entry; selection: Selection }) {
  if (!selection.selecting) return null;
  return (
    <input
      type="checkbox"
      className="np-pick"
      checked={selection.picked.has(entry.node.node_id)}
      disabled={!selection.selectable(entry)}
      aria-label={`选中 ${nodeLabel(entry.node)}`}
      onChange={() => selection.toggle(entry.node.node_id)}
      onClick={event => event.stopPropagation()}
    />
  );
}

function Flag({ node }: { node: NodeAgentStateItem }) {
  return (
    <span className="np-flag" title={node.public_ipv4_country ?? '未识别地区'}>
      {node.public_ipv4_country ? <RegionFlag code={node.public_ipv4_country} /> : <i className="np-flag-none" />}
    </span>
  );
}

/** 机器身份牌：与详情页同一块方形旗板；异常时右下角出现状态点，正常不画灯。 */
function Plate({ entry, size }: { entry: Entry; size: number }) {
  const { node } = entry;
  const flagged = !!node.public_ipv4_country;
  return (
    <span className={`nd-idplate np-plate${flagged ? '' : ' empty'}`} style={{ width: size, height: size }}>
      <span className="nd-idplate-clip">
        {flagged ? <RegionFlag code={node.public_ipv4_country} square /> : <Icon of="nodes" size={14} />}
      </span>
      {entry.status && !entry.retired && <i className={`node-lamp ${entry.tone}`} aria-hidden="true" />}
    </span>
  );
}

function Name({ entry }: { entry: Entry }) {
  return (
    <b
      className={`np-name${entry.node.name ? '' : ' mono'}`}
      title={entry.status ? `${nodeLabel(entry.node)} · ${entry.why}` : nodeLabel(entry.node)}
    >
      {nodeLabel(entry.node)}
    </b>
  );
}

function StatusText({ entry, className = '' }: { entry: Entry; className?: string }) {
  if (!entry.status) return null;
  const tone = entry.tone;
  return (
    <span className={`np-status ${tone} ${className}`} title={entry.why}>
      {!entry.retired && tone !== 'idle' && (
        <Icon of={tone === 'bad' ? 'ban' : 'warn'} size={12} className="np-status-icon" />
      )}
      <span>{entry.status}</span>
    </span>
  );
}

function bpsParts(value: number): Reading {
  const reading = observeBpsReadingParts(value);
  return { number: reading.number, unit: reading.unit };
}

function Rate({ entry, className = 'np-rate' }: { entry: Entry; className?: string }) {
  if (!entry.rate) return <span className={`${className} none`}>—</span>;
  const total = bpsParts(entry.rate.total);
  const rx = bpsParts(entry.rate.rx);
  const tx = bpsParts(entry.rate.tx);
  return (
    <span className={className} title={`网卡 30 秒均值 · 接收 ${rx.number} ${rx.unit} · 发送 ${tx.number} ${tx.unit}`}>
      {total.number}
      <small>{total.unit}</small>
    </span>
  );
}

const KiB = 1024;
const MiB = KiB ** 2;
const GiB = KiB ** 3;
const TiB = KiB ** 4;
const sig = (value: number) => (value >= 100 ? value.toFixed(0) : value >= 10 ? value.toFixed(1) : value.toFixed(2));

function bytesParts(value: number): Reading {
  if (value >= TiB) return { number: sig(value / TiB), unit: 'TiB' };
  if (value >= GiB) return { number: sig(value / GiB), unit: 'GiB' };
  if (value >= MiB) return { number: sig(value / MiB), unit: 'MiB' };
  return { number: String(Math.round(value / KiB)), unit: 'KiB' };
}
const bytesText = (value: number) => {
  const reading = bytesParts(value);
  return `${reading.number} ${reading.unit}`;
};
/** 额度是人填的整数居多：1 TiB 不写成 1.00 TiB。 */
const quotaText = (value: number) => {
  const reading = bytesParts(value);
  return `${reading.number.replace(/\.?0+$/, '')} ${reading.unit}`;
};

function quotaTone(share: number) {
  return share >= 1 ? 'bad' : share >= 0.85 ? 'warn' : 'ok';
}

function QuotaBar({ share }: { share: number }) {
  return (
    <span className={`np-bar ${quotaTone(share)}`} aria-hidden="true">
      <i style={{ width: `${Math.min(1, share) * 100}%` }} />
    </span>
  );
}

function QuotaRing({ share }: { share: number }) {
  const percent = Math.min(100, Math.max(0, share * 100));
  return (
    <svg
      className={`user-quota-ring np-ring ${quotaTone(share)}`}
      width="12"
      height="12"
      viewBox="0 0 12 12"
      aria-hidden="true"
    >
      <circle className="track" cx="6" cy="6" r="4.5" />
      {percent > 0 && (
        <circle
          className="value"
          cx="6"
          cy="6"
          r="4.5"
          pathLength="100"
          strokeDasharray={`${percent} ${100 - percent}`}
          transform="rotate(-90 6 6)"
        />
      )}
    </svg>
  );
}

const percentText = (share: number) => `${Math.round(share * 100)}%`;

function monthTitle(month: NonNullable<Entry['month']>) {
  return month.quota
    ? `本月 ${bytesText(month.used)}，额度 ${quotaText(month.quota)}（${percentText(month.used / month.quota)}）`
    : `本月 ${bytesText(month.used)}，不限额度`;
}

function Latency({ entry, className = '' }: { entry: Entry; className?: string }) {
  if (!entry.probes) return <Address node={entry.node} className={className} />;
  const title = entry.probes
    .map(probe => `${probe.name} ${probe.lost ? '超时' : probe.ms == null ? '—' : `${Math.round(probe.ms)} ms`}`)
    .join(' · ');
  return (
    <span className={`np-latency${entry.probesStale ? ' stale' : ''} ${className}`} title={title}>
      {entry.probes.slice(0, 3).map((probe, index) => (
        <span key={`${probe.name}-${index}`} className={probe.lost ? 'lost' : undefined}>
          {probe.lost || probe.ms == null ? '—' : Math.round(probe.ms)}
        </span>
      ))}
      <small>ms</small>
    </span>
  );
}

function Address({ node, className = '' }: { node: NodeAgentStateItem; className?: string }) {
  const { public_ipv4: v4, public_ipv6: v6 } = node;
  if (!v4 && !v6) return <span className={`np-addr none ${className}`}>仅 overlay</span>;
  return (
    <span className={`np-addr ${className}`} title={[v4, v6].filter(Boolean).join('  ')}>
      {v4 ?? v6}
    </span>
  );
}

/** 近 12 分钟网卡走势：面积加曲线，按本机自身峰值缩放，缺口断开。 */
function Spark({ entry, height }: { entry: Entry; height: number }) {
  const gradient = `np-g-${useId().replace(/[^a-zA-Z0-9_-]/g, '')}`;
  const values = entry.series;
  if (values.filter(value => value !== null).length < 2) {
    return <span className="np-spark empty" style={{ height }} aria-hidden="true" />;
  }
  const peak = Math.max(MIN_SCALE_BPS, ...values.map(value => value ?? 0)) * 1.08;
  const { areas, bases, lines } = sparklinePaths(values, peak, height);
  const tone = entry.retired ? 'idle' : entry.tone;
  return (
    <svg
      className={`np-spark tone-${tone}`}
      viewBox={`0 0 ${values.length - 1} ${height}`}
      preserveAspectRatio="none"
      style={{ height }}
      aria-hidden="true"
    >
      <defs>
        <linearGradient id={gradient} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor="currentColor" stopOpacity="0.28" />
          <stop offset="1" stopColor="currentColor" stopOpacity="0.02" />
        </linearGradient>
      </defs>
      {areas.map((path, index) => (
        <path key={`a${index}`} className="area" d={path} fill={`url(#${gradient})`} />
      ))}
      {bases.map((path, index) => (
        <path key={`b${index}`} className="base" d={path} />
      ))}
      {lines.map((path, index) => (
        <path key={`l${index}`} className="line" d={path} />
      ))}
    </svg>
  );
}

const entryClass = (base: string, entry: Entry, selection: Selection) =>
  `${base} tone-${entry.retired ? 'off' : entry.tone}${entry.retired ? ' off' : ''}${
    selection.picked.has(entry.node.node_id) ? ' picked' : ''
  }${selection.selecting ? ' selecting' : ''}`;

/* ══ A 小组件：卡片网格，按「身份 → 当下 → 本月」三层重排 ══ */

function Widget({ entry, fleet, selection }: { entry: Entry; fleet: Fleet; selection: Selection }) {
  const open = useOpen(fleet, selection, entry);
  const { month } = entry;
  const share = month?.quota ? month.used / month.quota : null;
  return (
    <article {...open} className={entryClass('np-widget', entry, selection)}>
      <div className="np-w-head">
        <PickBox entry={entry} selection={selection} />
        <Flag node={entry.node} />
        <Name entry={entry} />
        {entry.status ? <StatusText entry={entry} /> : <Latency entry={entry} />}
      </div>
      <div className="np-w-live">
        <Rate entry={entry} />
        <Spark entry={entry} height={34} />
      </div>
      <div className="np-w-month" title={month ? monthTitle(month) : undefined}>
        <span className="np-w-month-label">本月</span>
        {month ? (
          <>
            <b>{bytesText(month.used)}</b>
            {month.quota && <span className="np-w-quota">/ {quotaText(month.quota)}</span>}
            <span className="sp" />
            {share !== null ? (
              <span className={`np-pct ${quotaTone(share)}`}>{percentText(share)}</span>
            ) : (
              <span className="np-unlimited">不限额度</span>
            )}
          </>
        ) : (
          <b className="none">—</b>
        )}
      </div>
      {share !== null && <QuotaBar share={share} />}
    </article>
  );
}

function WidgetGrid({ fleet, selection }: { fleet: Fleet; selection: Selection }) {
  return (
    <div className="np-widgets">
      {fleet.entries.map(entry => (
        <Widget key={entry.node.node_id} entry={entry} fleet={fleet} selection={selection} />
      ))}
    </div>
  );
}

/* ══ B 清单：一行一台，列对齐 ══ */

function ListRow({
  entry,
  fleet,
  selection,
  targets,
}: {
  entry: Entry;
  fleet: Fleet;
  selection: Selection;
  targets: string[];
}) {
  const open = useOpen(fleet, selection, entry);
  const { month } = entry;
  const share = month?.quota ? month.used / month.quota : null;
  const probeOf = (name: string) => entry.probes?.find(probe => probe.name === name);
  return (
    <div {...open} role="row" className={entryClass('np-row', entry, selection)}>
      {selection.selecting && (
        <span className="np-c-pick" role="cell">
          <PickBox entry={entry} selection={selection} />
        </span>
      )}
      <span className="np-c-flag" role="cell">
        <Flag node={entry.node} />
      </span>
      <span className="np-c-name" role="cell">
        <Name entry={entry} />
        {entry.status ? <StatusText entry={entry} /> : <Address node={entry.node} />}
      </span>
      <span className="np-c-wave" role="cell">
        <Spark entry={entry} height={26} />
      </span>
      <span className="np-c-rate" role="cell">
        <Rate entry={entry} />
      </span>
      <span className="np-c-month" role="cell" title={month ? monthTitle(month) : undefined}>
        {month ? (
          <>
            <span className="np-month-line">
              <b>{bytesText(month.used)}</b>
              {share !== null && <span className={`np-pct ${quotaTone(share)}`}>{percentText(share)}</span>}
            </span>
            {share !== null ? <QuotaBar share={share} /> : <span className="np-unlimited">不限</span>}
          </>
        ) : (
          <b className="none">—</b>
        )}
      </span>
      {targets.map(name => {
        const probe = probeOf(name);
        return (
          <span
            key={name}
            role="cell"
            className={`np-c-ping${entry.probesStale ? ' stale' : ''}${probe?.lost ? ' lost' : ''}`}
            title={
              probe ? `${name} ${probe.lost ? '超时' : probe.ms == null ? '—' : `${Math.round(probe.ms)} ms`}` : name
            }
          >
            {!probe || probe.lost || probe.ms == null ? (
              '—'
            ) : (
              <>
                {Math.round(probe.ms)}
                <small>ms</small>
              </>
            )}
          </span>
        );
      })}
      <span className="np-c-go" role="cell" aria-hidden="true">
        <i className="np-chevron" />
      </span>
    </div>
  );
}

function FleetList({ fleet, selection }: { fleet: Fleet; selection: Selection }) {
  const targets = fleet.targets.slice(0, 3);
  const columns = (wave: boolean) =>
    [
      selection.selecting ? '18px' : null,
      '20px',
      'minmax(180px, 1fr)',
      wave ? 'minmax(120px, 240px)' : null,
      '96px',
      '150px',
      ...targets.map(() => '64px'),
      '12px',
    ]
      .filter(Boolean)
      .join(' ');
  return (
    <div
      className="np-list"
      role="table"
      aria-label="机器"
      style={{ ['--np-cols' as string]: columns(true), ['--np-cols-narrow' as string]: columns(false) }}
    >
      <div className="np-list-head" role="row">
        {selection.selecting && <span role="columnheader" />}
        <span role="columnheader" />
        <span role="columnheader">机器</span>
        <span role="columnheader" className="np-c-wave">
          近 12 分钟
        </span>
        <span role="columnheader" className="np-c-rate">
          当前
        </span>
        <span role="columnheader" className="np-c-month">
          本月
        </span>
        {targets.map(name => (
          <span role="columnheader" key={name} className="np-c-ping" title={`${name} TCP 延迟`}>
            {name}
          </span>
        ))}
        <span role="columnheader" />
      </div>
      {fleet.entries.map(entry => (
        <ListRow key={entry.node.node_id} entry={entry} fleet={fleet} selection={selection} targets={targets} />
      ))}
    </div>
  );
}

/* ══ C 磁贴：一屏放下整个机队 ══ */

function Tile({ entry, fleet, selection }: { entry: Entry; fleet: Fleet; selection: Selection }) {
  const open = useOpen(fleet, selection, entry);
  const { month } = entry;
  const share = month?.quota ? month.used / month.quota : null;
  return (
    <article {...open} className={entryClass('np-tile', entry, selection)}>
      <Plate entry={entry} size={34} />
      <span className="np-tile-text">
        <span className="np-tile-name">
          <PickBox entry={entry} selection={selection} />
          <Name entry={entry} />
        </span>
        <span className="np-tile-line">
          {entry.status ? <StatusText entry={entry} /> : <Rate entry={entry} className="np-tile-rate" />}
          {month && !entry.retired && (
            <span className="np-tile-month" title={monthTitle(month)}>
              {share !== null ? (
                <>
                  <QuotaRing share={share} />
                  <span className={`np-pct ${quotaTone(share)}`}>{percentText(share)}</span>
                </>
              ) : (
                <span className="np-tile-bytes">{bytesText(month.used)}</span>
              )}
            </span>
          )}
        </span>
      </span>
    </article>
  );
}

function TileGrid({ fleet, selection }: { fleet: Fleet; selection: Selection }) {
  return (
    <div className="np-tiles">
      {fleet.entries.map(entry => (
        <Tile key={entry.node.node_id} entry={entry} fleet={fleet} selection={selection} />
      ))}
    </div>
  );
}

/* ══ D 名册：与用户页同一套双栏，右侧是生产详情 ══ */

function RosterRow({
  entry,
  fleet,
  selection,
  current,
}: {
  entry: Entry;
  fleet: Fleet;
  selection: Selection;
  current: boolean;
}) {
  const open = useOpen(fleet, selection, entry, id => navigateInPlace('nodes', { p: 'node', id }));
  const { month } = entry;
  const share = month?.quota ? month.used / month.quota : null;
  return (
    <div
      {...open}
      role="option"
      aria-selected={current}
      className={`user-row np-roster-row${entry.retired ? ' off' : ''}${current ? ' picked' : ''}${
        selection.picked.has(entry.node.node_id) ? ' checked' : ''
      }`}
    >
      <Plate entry={entry} size={34} />
      <span className="user-row-name">
        <PickBox entry={entry} selection={selection} />
        <Name entry={entry} />
      </span>
      <span className={`user-row-usage${entry.rate ? '' : ' none'}`}>
        <Rate entry={entry} className="np-roster-rate" />
      </span>
      <span className="user-row-meta">{entry.status ? <StatusText entry={entry} /> : <Latency entry={entry} />}</span>
      <span className="user-row-quota" title={month ? monthTitle(month) : undefined}>
        {month && share !== null ? (
          <>
            <QuotaRing share={share} />
            <span className={share >= 1 ? 'over' : undefined}>{percentText(share)}</span>
          </>
        ) : month ? (
          <span className="np-roster-month">{bytesText(month.used)}</span>
        ) : null}
      </span>
    </div>
  );
}

function fakeWin(drill: Drill): Win {
  return { id: -1, key: 'mock:nodes-detail', title: '机器', data: { drill } } as unknown as Win;
}

function SplitPage({ fleet, selection, drill }: { fleet: Fleet; selection: Selection; drill: Drill }) {
  const narrow = useNarrow();
  const routed = drill.p === 'node' ? drill.id : null;
  // 列表页默认选中第一台在线机器：从未上报或已退役的机器详情大多是空的。
  const fallback =
    fleet.entries.find(entry => !entry.retired && entry.tone !== 'idle')?.node.node_id ??
    fleet.entries[0]?.node.node_id;
  const selected = routed ?? (narrow ? null : (fallback ?? null));
  const detailDrill = useMemo<Drill | null>(
    () => (drill.p === 'node' ? drill : selected ? { p: 'node', id: selected } : null),
    [drill, selected],
  );
  const win = useMemo(() => (detailDrill ? fakeWin(detailDrill) : null), [detailDrill]);
  return (
    <div className="cardpage user-cardpage np-split-page">
      <div className={`user-split np-split${routed ? ' np-detail-route' : ' np-list-route'}`}>
        <section className="panel titled user-list-panel user-split-roster np-roster" data-page-title="true">
          <TitleBar fleet={fleet} selection={selection} />
          {fleet.entries.length === 0 ? (
            <EmptyFleet />
          ) : (
            <div className="user-roster-options" role="listbox" aria-label="机器列表">
              {fleet.entries.map(entry => (
                <RosterRow
                  key={entry.node.node_id}
                  entry={entry}
                  fleet={fleet}
                  selection={selection}
                  current={entry.node.node_id === selected}
                />
              ))}
            </div>
          )}
        </section>
        {win && (
          <div className="np-split-detail">
            <NodesPane key={selected ?? ''} win={win} bare />
          </div>
        )}
      </div>
    </div>
  );
}

/* ══ 页面 ══ */

function Loading() {
  return <div className="np-loading" aria-busy="true" />;
}

export function MockMachinePage({ option, drill }: { option: Option; drill: Drill }) {
  const fleet = useFleet();
  const selection = useSelection(fleet.entries);
  if (fleet.nodes.isPending) return <Loading />;
  if (option === 'd') return <SplitPage fleet={fleet} selection={selection} drill={drill} />;
  const body: ReactNode =
    fleet.entries.length === 0 ? (
      <EmptyFleet />
    ) : option === 'a' ? (
      <WidgetGrid fleet={fleet} selection={selection} />
    ) : option === 'b' ? (
      <FleetList fleet={fleet} selection={selection} />
    ) : (
      <TileGrid fleet={fleet} selection={selection} />
    );
  return (
    <div className={`cardpage node-cardpage np-page np-${option}`}>
      <section className="panel titled node-list-panel" data-page-title="true">
        <TitleBar fleet={fleet} selection={selection} />
        {body}
      </section>
    </div>
  );
}
