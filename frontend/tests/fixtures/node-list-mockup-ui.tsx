// Option pages for the machine list mockup (机器页). Never imported by the app.
//
// Every option uses the machine detail's page head verbatim: the continuous sheet (.nd-sheet > .nd-paper), the
// identity block (.nd-page-identity with an .nd-idplate), the tab beam (.nd-tabs) and the tools (.nd-tools).
// The cards are the production NodeCard markup on the production queries; only the arrangement differs.
import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
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
} from '../../src/api';
import { navigate } from '../../src/forge/route';
import {
  NicMonthTotal,
  NicWave,
  TcpProbeLatest,
  exactBytes,
  nodeLampState,
  nodeListSnapshotFresh,
  nodeRemovalReady,
  prefetchNodeDetailData,
} from '../../src/panes/nodes';
import { can, isVisitor, useSession } from '../../src/session';
import { EmptyState, Loading } from '../../src/ui/bits';
import { useNow } from '../../src/ui/clock';
import { Icon, type IconName } from '../../src/ui/icons';
import { useMetricQuery } from '../../src/ui/metric-cadence';
import { observeBpsReadingParts } from '../../src/ui/observe-chart';
import { RegionFlag } from '../../src/ui/region-flag';
import { sparklinePaths } from '../../src/ui/sparkline';
import { useNarrow } from '../../src/ui/viewport';
import type { Machine, SceneKey } from './node-list-mockup-data';

export type Option = 'a' | 'b' | 'c' | 'd';

/* ══ 与生产列表相同的取数与判定 ══ */

const LIST_NIC_WINDOWS = 24;
const NAME_ORDER = new Intl.Collator('zh', { numeric: true, sensitivity: 'base' });
const REGION_NAMES = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const regionName = (code: string) => (code ? (REGION_NAMES.of(code) ?? code) : '未识别');
const nodeLabel = (node: NodeAgentStateItem) => node.name || node.node_id;

type Tone = 'ok' | 'warn' | 'bad' | 'idle';
interface Entry {
  node: NodeAgentStateItem;
  lamp: { tone: Tone; why: string };
  retired: boolean;
  /** 在线判定，与生产 NodeCard 的 `live` 相同：只用来给上报时间着色。 */
  live: 'ok' | 'bad' | 'idle';
}

function pollTone(node: NodeAgentStateItem): 'ok' | 'bad' | 'idle' {
  if (node.runtime_report_fresh) return 'ok';
  if (!node.last_poll_at) return 'idle';
  const raw = node.last_poll_at;
  const at = Date.parse(raw.endsWith('Z') || raw.includes('+') ? raw : `${raw}Z`);
  if (Number.isNaN(at)) return 'idle';
  return Date.now() - at > 60_000 ? 'bad' : 'ok';
}

function useFleet() {
  const qc = useQueryClient();
  const { who } = useSession();
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
  const fresh = nodeListSnapshotFresh(nodes.dataUpdatedAt);
  const entries = useMemo<Entry[]>(() => {
    const list = [...(nodes.data?.nodes ?? [])].sort(
      (a, b) =>
        Number(!!a.retired_at) - Number(!!b.retired_at) ||
        NAME_ORDER.compare(nodeLabel(a), nodeLabel(b)) ||
        NAME_ORDER.compare(a.node_id, b.node_id),
    );
    const isolated = new Set(list.filter(node => node.operationally_isolated).map(node => node.node_id));
    return list.map(node => ({
      node,
      retired: !!node.retired_at,
      live: fresh ? pollTone(node) : 'idle',
      lamp: fresh ? nodeLampState(node, undefined, isolated) : { tone: 'idle', why: '缓存状态待确认' },
    }));
  }, [nodes.data, fresh]);
  const trafficOf = useMemo(
    () => new Map<string, NodeTrafficItem>((traffic.data?.nodes ?? []).map(item => [item.node_id, item])),
    [traffic.data],
  );
  const loadOf = useMemo(
    () => new Map<string, NodeNicView>((load.data?.nodes ?? []).map(item => [item.node_id, item])),
    [load.data],
  );
  const pingOf = useMemo(
    () => new Map<string, NodePingProbeLatestView>((ping.data?.nodes ?? []).map(item => [item.node_id, item])),
    [ping.data],
  );
  const live = entries.filter(entry => !entry.retired);
  return {
    nodes,
    entries,
    live,
    retired: entries.filter(entry => entry.retired),
    attention: live.filter(entry => entry.lamp.tone === 'warn' || entry.lamp.tone === 'bad'),
    removable: entries.filter(entry => entry.retired && nodeRemovalReady(entry.node)),
    trafficOf,
    loadOf,
    pingOf,
    pingInterval: ping.data?.interval_secs ?? 60,
    pingReady: ping.isSuccess,
    trafficPending: traffic.isPending,
    prepare: (id: string) => void prefetchNodeDetailData(qc, id, !isVisitor(who)),
  };
}
type Fleet = ReturnType<typeof useFleet>;

/* ══ 机器卡：生产 NodeCard 的同一份结构 ══ */

function PollAgo({ at }: { at: string | null }) {
  const now = useNow();
  if (!at) return <>—</>;
  const time = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  if (Number.isNaN(time)) return <>—</>;
  const secs = Math.max(0, Math.round((now - time) / 1000));
  const text =
    secs < 60
      ? `${secs}s`
      : secs < 3600
        ? `${Math.floor(secs / 60)}m`
        : secs < 86400
          ? `${Math.floor(secs / 3600)}h`
          : `${Math.floor(secs / 86400)}d`;
  return <span title={at}>{text}</span>;
}

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

function NodeAddr({ node }: { node: NodeAgentStateItem }) {
  const { public_ipv4: v4, public_ipv6: v6 } = node;
  if (!v4 && !v6) return <span className="nc-ip none">仅 overlay</span>;
  return (
    <span className="nc-ip" title={[v4, v6].filter(Boolean).join('  ')}>
      {v4 ?? v6}
    </span>
  );
}

/** 卡片与列表行共用的读数：配置了 TCP 目标时显示延迟，否则显示地址。 */
function CornerReading({ entry, fleet }: { entry: Entry; fleet: Fleet }) {
  const view = fleet.pingOf.get(entry.node.node_id);
  return !fleet.pingReady || (view && view.targets.some(target => target.kind === 'tcp')) ? (
    <TcpProbeLatest view={view} intervalSecs={fleet.pingInterval} pending={!fleet.pingReady} />
  ) : (
    <NodeAddr node={entry.node} />
  );
}

const openNode = (id: string) => navigate('nodes', { p: 'node', id });

function usePrepare(fleet: Fleet, id: string, disabled: boolean) {
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const now = () => {
    if (disabled) return;
    window.clearTimeout(timer.current);
    timer.current = 0;
    fleet.prepare(id);
  };
  return {
    now,
    schedule: () => {
      if (disabled || timer.current) return;
      timer.current = window.setTimeout(now, 120);
    },
    cancel: () => {
      window.clearTimeout(timer.current);
      timer.current = 0;
    },
  };
}

interface Pick {
  selecting: boolean;
  selectable: boolean;
  checked: boolean;
  onPick: () => void;
}

function FleetCard({ entry, fleet, pick, why = false }: { entry: Entry; fleet: Fleet; pick: Pick; why?: boolean }) {
  const { who } = useSession();
  const narrow = useNarrow();
  const { node, lamp } = entry;
  const retired = node.lifecycle_phase !== 'active';
  const prepare = usePrepare(fleet, node.node_id, pick.selecting);
  const open = () => {
    if (pick.selecting) return pick.selectable && pick.onPick();
    prepare.now();
    openNode(node.node_id);
  };
  return (
    <article
      role="button"
      tabIndex={0}
      className={`ncard tone-${lamp.tone}${retired ? ' off' : ''}${pick.checked ? ' picked' : ''}`}
      onMouseEnter={prepare.schedule}
      onMouseLeave={prepare.cancel}
      onClick={open}
      onKeyDown={event => {
        if (event.target !== event.currentTarget) return;
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          open();
        }
      }}
    >
      <div className="nc-head">
        {pick.selecting && (
          <span className="nc-pick-slot">
            <input
              type="checkbox"
              className="nc-pick"
              checked={pick.checked}
              disabled={!pick.selectable}
              aria-label={`选中 ${nodeLabel(node)}`}
              onChange={pick.onPick}
              onClick={event => event.stopPropagation()}
            />
          </span>
        )}
        <span className={`nc-status-slot${node.public_ipv4_country ? ' with-region' : ''}`}>
          <i className={`node-lamp ${lamp.tone}`} title={lamp.why} aria-label={lamp.why} />
        </span>
        {node.public_ipv4_country && (
          <span className="nc-region-flag">
            <RegionFlag code={node.public_ipv4_country} />
          </span>
        )}
        <b className={node.name ? undefined : 'mono'}>{nodeLabel(node)}</b>
        <span className={`nc-ago${entry.live === 'bad' && !retired ? ' bad' : ''}`}>
          {retired ? lifecycleLabel(node) : <PollAgo at={node.last_poll_at} />}
        </span>
      </div>
      {why && <p className={`nl-why ${lamp.tone}`}>{lamp.why}</p>}
      <NicWave load={fleet.loadOf.get(node.node_id)} />
      <div className="nc-foot">
        <NicMonthTotal traffic={fleet.trafficOf.get(node.node_id)} pending={fleet.trafficPending} retired={retired} />
        <span className="sp" />
        <span className="nc-dock" onClick={event => event.stopPropagation()}>
          {!narrow && !retired && (
            <button
              className="btn"
              disabled={!can(who.role, 'edit')}
              onClick={() => navigate('nodes', { p: 'chain', id: node.node_id })}
            >
              建一条链
            </button>
          )}
        </span>
        <CornerReading entry={entry} fleet={fleet} />
      </div>
    </article>
  );
}

/* ══ 页头：与机器详情同一条横梁 ══ */

function fleetTone(live: Entry[]): { tone: Tone; why: string } {
  if (live.length === 0) return { tone: 'idle', why: '没有在用机器' };
  const bad = live.filter(entry => entry.lamp.tone === 'bad').length;
  const warn = live.filter(entry => entry.lamp.tone === 'warn').length;
  const why = [bad > 0 && `${bad} 台需立即查看`, warn > 0 && `${warn} 台有警告`].filter(Boolean).join(' · ');
  return { tone: bad > 0 ? 'bad' : warn > 0 ? 'warn' : 'ok', why: why || '全部正常' };
}

/** 页面的身份牌：详情页是地区旗加机器状态灯，列表页是「机器」图标加整批状态灯。 */
function FleetIdentity({ fleet, meta }: { fleet: Fleet; meta: ReactNode }) {
  const tone = fleetTone(fleet.live);
  return (
    <div className="nd-page-identity">
      <span className="nd-idplate empty nl-plate" title={tone.why}>
        <span className="nd-idplate-clip">
          <Icon of="nodes" size={15} />
        </span>
        <i className={`node-lamp ${tone.tone}`} aria-label={tone.why} />
      </span>
      <div className="nd-ident-text">
        <div className="nd-ident-row">
          <h1 className="nd-id nd-name">机器</h1>
        </div>
        {meta && <span className="nd-ident-meta nl-meta">{meta}</span>}
      </div>
    </div>
  );
}

const regionCount = (entries: Entry[]) => new Set(entries.map(entry => entry.node.public_ipv4_country ?? '')).size;

interface MenuItem {
  icon: IconName;
  label: string;
  note?: string;
  danger?: boolean;
  disabled?: boolean;
  rule?: boolean;
  onSelect: () => void;
}

/** 页头的 ⋯ 菜单，开合与定位同机器详情（document 级点击与 Escape 关闭，菜单贴触发钮右缘）。 */
function MoreMenu({ items, label = '更多操作' }: { items: MenuItem[]; label?: string }) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    const escape = (event: KeyboardEvent) => event.key === 'Escape' && close();
    document.addEventListener('click', close);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', escape);
    };
  }, [open]);
  useLayoutEffect(() => {
    if (!open) return;
    const box = trigger.current?.getBoundingClientRect();
    const element = menu.current;
    if (!box || !element) return;
    element.style.top = `${box.bottom + 6}px`;
    element.style.left = `${Math.min(innerWidth - element.offsetWidth - 10, Math.max(10, box.right - element.offsetWidth))}px`;
  }, [open]);
  return (
    <div className="fg-menuwrap">
      <button
        ref={trigger}
        type="button"
        className="btn fg-more"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={event => {
          event.stopPropagation();
          setOpen(value => !value);
        }}
      >
        <Icon of="more" size={14} className="nd-tool-icon" />
      </button>
      {open &&
        createPortal(
          <div
            ref={menu}
            className="fg-menu action-menu"
            data-node-action-menu
            role="menu"
            onClick={() => setOpen(false)}
          >
            {items.map((item, index) => (
              <Fragment key={item.label}>
                {item.rule && index > 0 && <hr />}
                <button
                  role="menuitem"
                  className={item.danger ? 'dg' : undefined}
                  disabled={item.disabled}
                  onClick={item.onSelect}
                >
                  <Icon of={item.icon} size={14} className="action-menu-icon" />
                  <span>
                    {item.label}
                    {item.note && <small>{item.note}</small>}
                  </span>
                </button>
              </Fragment>
            ))}
          </div>,
          document.body,
        )}
    </div>
  );
}

type Tab = 'live' | 'attention' | 'retired';

/** 与详情页签同一套：图标 + 名称 + 角标；金色数字＝有几处要看，灰色数字＝条目数。 */
function FleetTabs({ tab, onTab, fleet }: { tab: Tab; onTab: (next: Tab) => void; fleet: Fleet }) {
  const tabs: { value: Tab; icon: IconName; label: string; badge?: ReactNode }[] = [
    { value: 'live', icon: 'servers', label: '在用' },
    {
      value: 'attention',
      icon: 'warn',
      label: '需处理',
      badge: fleet.attention.length > 0 && <span className="nd-tab-badge gold">{fleet.attention.length}</span>,
    },
  ];
  if (fleet.retired.length > 0)
    tabs.push({
      value: 'retired',
      icon: 'dash',
      label: '退役',
      badge: <span className="nd-tab-badge dim">{fleet.retired.length}</span>,
    });
  const move = (event: ReactKeyboardEvent<HTMLButtonElement>, index: number) => {
    const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
    if (!step) return;
    event.preventDefault();
    const next = tabs[(index + step + tabs.length) % tabs.length];
    onTab(next.value);
    requestAnimationFrame(() => document.getElementById(`nl-tab-${next.value}`)?.focus());
  };
  return (
    <div className="nd-tabs" role="tablist" aria-label="机器分组">
      <div className="nd-tabs-seg">
        {tabs.map((item, index) => (
          <button
            key={item.value}
            id={`nl-tab-${item.value}`}
            type="button"
            role="tab"
            aria-selected={tab === item.value}
            tabIndex={tab === item.value ? 0 : -1}
            onKeyDown={event => move(event, index)}
            onClick={() => onTab(item.value)}
          >
            <Icon of={item.icon} size={14} className="nd-tab-ic" />
            {item.label}
            {item.badge}
          </button>
        ))}
      </div>
    </div>
  );
}

/* ══ 选择：多选退役 / 多选移除 ══ */

type Selection = null | 'retire' | 'remove';

function useSelection() {
  const [mode, setMode] = useState<Selection>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  return {
    mode,
    picked,
    begin: (next: Exclude<Selection, null>) => {
      setMode(next);
      setPicked(new Set());
    },
    exit: () => {
      setMode(null);
      setPicked(new Set());
    },
    toggle: (id: string) =>
      setPicked(previous => {
        const next = new Set(previous);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      }),
  };
}
type SelectionState = ReturnType<typeof useSelection>;

const pickOf = (selection: SelectionState, entry: Entry): Pick => {
  const purpose = entry.retired ? 'remove' : 'retire';
  return {
    selecting: selection.mode === purpose,
    selectable: purpose === 'retire' ? true : nodeRemovalReady(entry.node),
    checked: selection.picked.has(entry.node.node_id),
    onPick: () => selection.toggle(entry.node.node_id),
  };
};

/** 右侧工具：平时是「纳管机器」和 ⋯；多选时换成取消与确认，同生产列表标题栏的切换。 */
function FleetTools({
  fleet,
  selection,
  showRetired,
  before,
}: {
  fleet: Fleet;
  selection: SelectionState;
  /** 当前看的是退役机器：⋯ 换成移除。 */
  showRetired: boolean;
  before?: ReactNode;
}) {
  const { who } = useSession();
  const system = can(who.role, 'system');
  const narrow = useNarrow();
  if (selection.mode) {
    const count = selection.picked.size;
    return (
      <div className="nd-tools">
        <div className="nd-acts">
          <button type="button" className="btn" onClick={selection.exit}>
            取消
          </button>
          {selection.mode === 'retire' ? (
            <button type="button" className="btn danger" disabled={count === 0} onClick={selection.exit}>
              退役下线{count > 0 ? ` ${count}` : ''}
            </button>
          ) : (
            <button
              type="button"
              className="btn danger node-remove-button"
              disabled={count === 0}
              onClick={selection.exit}
            >
              <Icon of="trash" size={13} className="node-remove-button-icon" />
              移除选中{count > 0 ? ` ${count}` : ''}
            </button>
          )}
        </div>
      </div>
    );
  }
  const items: MenuItem[] = showRetired
    ? [
        {
          icon: 'check',
          label: '多选移除',
          note: '勾选已停用的机器后永久移除',
          disabled: !system || fleet.removable.length === 0,
          onSelect: () => selection.begin('remove'),
        },
        {
          icon: 'trash',
          label: `移除全部 ${fleet.removable.length} 台`,
          note: '包含这些机器的线路一并删除，不能撤销',
          danger: true,
          disabled: !system || fleet.removable.length === 0,
          onSelect: () => undefined,
        },
      ]
    : [
        {
          icon: 'check',
          label: '多选退役',
          note: '勾选后退役下线：保留记录，为 Agent 创建完整停用发布',
          disabled: !system || fleet.live.length === 0,
          onSelect: () => selection.begin('retire'),
        },
      ];
  return (
    <div className="nd-tools">
      {before}
      <div className="nd-acts">
        <button
          type="button"
          className="btn primary nl-add"
          disabled={!system}
          onClick={() => navigate('nodes', { p: 'provision', step: 1 })}
        >
          <Icon of="plus" size={13} className="nd-tool-icon" />
          {narrow ? '纳管' : '纳管机器'}
        </button>
        <MoreMenu items={items} />
      </div>
    </div>
  );
}

/** 卡片视图的图标。生产图标集里还没有网格；笔画参数同 ui/icons.tsx 的 Icon，落地时加进 PATHS。 */
function GridIcon() {
  return (
    <span className="ui-icon nd-tool-icon" aria-hidden="true">
      <svg
        width={13}
        height={13}
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinejoin="round"
      >
        <rect x="2.4" y="2.4" width="4.6" height="4.6" rx="1.1" />
        <rect x="9" y="2.4" width="4.6" height="4.6" rx="1.1" />
        <rect x="2.4" y="9" width="4.6" height="4.6" rx="1.1" />
        <rect x="9" y="9" width="4.6" height="4.6" rx="1.1" />
      </svg>
    </span>
  );
}

/* ══ 正文排列 ══ */

function CardGrid({
  entries,
  fleet,
  selection,
  why,
}: {
  entries: Entry[];
  fleet: Fleet;
  selection: SelectionState;
  why?: boolean;
}) {
  return (
    <div className="ncards">
      {entries.map(entry => (
        <FleetCard key={entry.node.node_id} entry={entry} fleet={fleet} pick={pickOf(selection, entry)} why={why} />
      ))}
    </div>
  );
}

interface Region {
  code: string;
  entries: Entry[];
}

/** 按 IPv4 归属地分段：大的地区在前，同样大按名称；段内仍按机器名。 */
function regionsOf(entries: Entry[]): Region[] {
  const byCode = new Map<string, Entry[]>();
  for (const entry of entries) {
    const code = entry.node.public_ipv4_country ?? '';
    byCode.set(code, [...(byCode.get(code) ?? []), entry]);
  }
  return [...byCode.entries()]
    .map(([code, list]) => ({ code, entries: list }))
    .sort(
      (a, b) =>
        Number(a.code === '') - Number(b.code === '') ||
        b.entries.length - a.entries.length ||
        NAME_ORDER.compare(regionName(a.code), regionName(b.code)),
    );
}

/** 段宽随台数：一段最多占满一行，小的地区并排放进同一行（dense 回填），段内卡片与整页网格同一列宽。
 *  列数按容器宽度与生产卡片的最小宽度（268px）计算，同 `.ncards` 的 auto-fill。 */
function useShelfColumns() {
  const ref = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(4);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => setColumns(Math.max(1, Math.floor((element.clientWidth + 14) / (268 + 14))));
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return { ref, columns };
}

function RegionSections({
  entries,
  fleet,
  selection,
  why,
}: {
  entries: Entry[];
  fleet: Fleet;
  selection: SelectionState;
  why?: boolean;
}) {
  const shelf = useShelfColumns();
  return (
    <div
      ref={shelf.ref}
      className="nl-shelf"
      style={{ gridTemplateColumns: `repeat(${shelf.columns}, minmax(0, 1fr))` }}
    >
      {regionsOf(entries).map(region => {
        const span = Math.min(region.entries.length, shelf.columns);
        return (
          <section className="nl-shelf-group" key={region.code || 'none'} style={{ gridColumn: `span ${span}` }}>
            <h4 className="sec nl-shelf-head">
              <span className="nl-region-flag">
                {region.code ? <RegionFlag code={region.code} /> : <Icon of="region" size={13} />}
              </span>
              {regionName(region.code)}
              <span className="nl-region-count">{region.entries.length}</span>
            </h4>
            <div className="ncards" style={{ gridTemplateColumns: `repeat(${span}, minmax(0, 1fr))` }}>
              {region.entries.map(entry => (
                <FleetCard
                  key={entry.node.node_id}
                  entry={entry}
                  fleet={fleet}
                  pick={pickOf(selection, entry)}
                  why={why}
                />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}

/* 列表视图：一行一台，列对齐后可以纵向比较。走势用实时流量卡同一套迷你走势（sparklinePaths）。 */
const SPARK_HEIGHT = 22;

function NicSpark({ view }: { view?: NodeNicView }) {
  const samples = (view?.series ?? []).slice(-LIST_NIC_WINDOWS);
  if (samples.length < 2) return <span className="nl-spark none">尚无样本</span>;
  const rx = samples.map(sample => (sample.has_gap ? null : sample.nic_rx_bps));
  const tx = samples.map(sample => (sample.has_gap ? null : sample.nic_tx_bps));
  const peak = Math.max(1_000_000, ...rx.map(value => value ?? 0), ...tx.map(value => value ?? 0)) * 1.06;
  const down = sparklinePaths(rx, peak, SPARK_HEIGHT);
  const up = sparklinePaths(tx, peak, SPARK_HEIGHT);
  return (
    <svg
      className="rut-spark nl-spark"
      viewBox={`0 0 ${samples.length - 1} ${SPARK_HEIGHT}`}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      {down.bases.map((path, index) => (
        <path key={`b${index}`} className="rut-spark-base" d={path} />
      ))}
      {down.areas.map((path, index) => (
        <path key={`a${index}`} className="rut-spark-area" d={path} />
      ))}
      {up.lines.map((path, index) => (
        <path key={`u${index}`} className="rut-spark-up" d={path} />
      ))}
      {down.lines.map((path, index) => (
        <path key={`d${index}`} className="rut-spark-line" d={path} />
      ))}
    </svg>
  );
}

function RateNow({ view, live }: { view?: NodeNicView; live: Entry['live'] }) {
  const last = view?.series.at(-1);
  if (!last || last.has_gap || live === 'bad') return <span className="nl-rate none">—</span>;
  const total = last.nic_rx_bps + last.nic_tx_bps;
  const reading = observeBpsReadingParts(total);
  return (
    <span
      className="nl-rate"
      title={`接收 ${observeBpsReadingParts(last.nic_rx_bps).number} · 发送 ${observeBpsReadingParts(last.nic_tx_bps).number}`}
    >
      {reading.number}
      <small>{reading.unit}</small>
    </span>
  );
}

function MonthQuota({ traffic }: { traffic?: NodeTrafficItem }) {
  if (!traffic || (traffic.tracking_started_at_unix_secs === null && traffic.calibrated_at_unix_secs === null))
    return <span className="nl-month none">—</span>;
  const used = Number(traffic.total_bytes);
  const quota = traffic.quota_bytes ? Number(traffic.quota_bytes) : null;
  const share = quota ? used / quota : null;
  return (
    <span className="nl-month">
      <span className="nl-month-value">{exactBytes(traffic.total_bytes)}</span>
      {share !== null && (
        <span className={`nl-quota${share >= 0.9 ? ' hot' : ''}`} title={`额度 ${exactBytes(traffic.quota_bytes!)}`}>
          <span className="cap-bar">
            <i className={`u${share >= 1 ? ' x' : ''}`} style={{ left: 0, width: `${Math.min(1, share) * 100}%` }} />
          </span>
          <em>{Math.round(share * 100)}%</em>
        </span>
      )}
    </span>
  );
}

function FleetTable({ entries, fleet, selection }: { entries: Entry[]; fleet: Fleet; selection: SelectionState }) {
  const selecting = entries.some(entry => pickOf(selection, entry).selecting);
  return (
    <div className="nl-table-wrap">
      <table className="t nl-table">
        <thead>
          <tr>
            {selecting && <th className="nl-c-pick" aria-label="选择" />}
            <th className="nl-c-name">机器</th>
            <th className="nl-c-wave">网卡 · 近 12 分钟</th>
            <th className="nl-c-rate">当前</th>
            <th className="nl-c-month">本月</th>
            <th className="nl-c-ping">TCP 延迟</th>
            <th className="nl-c-ip">地址</th>
            <th className="nl-c-ago">上报</th>
          </tr>
        </thead>
        <tbody>
          {entries.map(entry => (
            <FleetRow
              key={entry.node.node_id}
              entry={entry}
              fleet={fleet}
              pick={pickOf(selection, entry)}
              selecting={selecting}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function FleetRow({ entry, fleet, pick, selecting }: { entry: Entry; fleet: Fleet; pick: Pick; selecting: boolean }) {
  const { node, lamp } = entry;
  const retired = node.lifecycle_phase !== 'active';
  const prepare = usePrepare(fleet, node.node_id, pick.selecting);
  const open = () => {
    if (pick.selecting) return pick.selectable && pick.onPick();
    prepare.now();
    openNode(node.node_id);
  };
  const notable = lamp.tone === 'warn' || lamp.tone === 'bad' || retired;
  return (
    <tr
      className={`rowlink tone-${lamp.tone}${retired ? ' off' : ''}${pick.checked ? ' picked' : ''}`}
      tabIndex={0}
      onMouseEnter={prepare.schedule}
      onMouseLeave={prepare.cancel}
      onClick={open}
      onKeyDown={event => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          open();
        }
      }}
    >
      {selecting && (
        <td className="nl-c-pick">
          {pick.selecting && (
            <input
              type="checkbox"
              className="nc-pick"
              checked={pick.checked}
              disabled={!pick.selectable}
              aria-label={`选中 ${nodeLabel(node)}`}
              onChange={pick.onPick}
              onClick={event => event.stopPropagation()}
            />
          )}
        </td>
      )}
      <td className="nl-c-name">
        <span className="nl-name">
          <i className={`node-lamp ${lamp.tone}`} title={lamp.why} aria-label={lamp.why} />
          <span className="nl-name-flag">
            {node.public_ipv4_country && <RegionFlag code={node.public_ipv4_country} />}
          </span>
          <b>{nodeLabel(node)}</b>
          {notable && (
            <span className={`nl-name-why ${retired ? 'idle' : lamp.tone}`}>
              {retired ? lifecycleLabel(node) : lamp.why}
            </span>
          )}
        </span>
      </td>
      <td className="nl-c-wave">
        <NicSpark view={fleet.loadOf.get(node.node_id)} />
      </td>
      <td className="nl-c-rate">
        <RateNow view={fleet.loadOf.get(node.node_id)} live={entry.live} />
      </td>
      <td className="nl-c-month">
        <MonthQuota traffic={fleet.trafficOf.get(node.node_id)} />
      </td>
      <td className="nl-c-ping">
        <TcpProbeLatest
          view={fleet.pingOf.get(node.node_id)}
          intervalSecs={fleet.pingInterval}
          pending={!fleet.pingReady}
        />
      </td>
      <td className="nl-c-ip">
        <NodeAddr node={node} />
      </td>
      <td className={`nl-c-ago${entry.live === 'bad' && !retired ? ' bad' : ''}`}>
        {retired ? '—' : <PollAgo at={node.last_poll_at} />}
      </td>
    </tr>
  );
}

/* D：侧栏分组。状态与地区放在同一列导航里，选中项同页签的浅面；手机上横排成一行。 */
interface Group {
  key: string;
  label: ReactNode;
  count: number;
  tone?: 'gold' | 'err' | 'dim';
  entries: Entry[];
}

function railGroups(fleet: Fleet): { status: Group[]; regions: Group[] } {
  const offline = fleet.live.filter(entry => entry.lamp.why === '失联');
  const never = fleet.live.filter(entry => entry.lamp.why === '从未上报');
  const status: Group[] = [
    { key: 'all', label: '全部在用', count: fleet.live.length, entries: fleet.live, tone: 'dim' },
    { key: 'attention', label: '需处理', count: fleet.attention.length, entries: fleet.attention, tone: 'gold' },
  ];
  if (offline.length > 0)
    status.push({ key: 'offline', label: '失联', count: offline.length, entries: offline, tone: 'err' });
  if (never.length > 0)
    status.push({ key: 'never', label: '未上报', count: never.length, entries: never, tone: 'dim' });
  if (fleet.retired.length > 0)
    status.push({ key: 'retired', label: '退役', count: fleet.retired.length, entries: fleet.retired, tone: 'dim' });
  const regions = regionsOf(fleet.live).map(region => ({
    key: `region:${region.code}`,
    label: (
      <>
        <span className="nl-rail-flag">
          {region.code ? <RegionFlag code={region.code} /> : <Icon of="region" size={12} />}
        </span>
        {regionName(region.code)}
      </>
    ),
    count: region.entries.length,
    tone: 'dim' as const,
    entries: region.entries,
  }));
  return { status, regions };
}

function Rail({
  groups,
  value,
  onChange,
}: {
  groups: { status: Group[]; regions: Group[] };
  value: string;
  onChange: (key: string) => void;
}) {
  const item = (group: Group) => (
    <button
      key={group.key}
      type="button"
      className="nl-rail-item"
      aria-pressed={value === group.key}
      onClick={() => onChange(group.key)}
    >
      <span className="nl-rail-label">{group.label}</span>
      {group.count > 0 && <span className={`nd-tab-badge ${group.tone ?? 'dim'}`}>{group.count}</span>}
    </button>
  );
  return (
    <nav className="nl-rail" aria-label="机器分组">
      <div className="nl-rail-sec">{groups.status.map(item)}</div>
      <div className="nl-rail-sec">
        <span className="nl-rail-title">地区</span>
        {groups.regions.map(item)}
      </div>
    </nav>
  );
}

function matches(entry: Entry, query: string) {
  const text = query.trim().toLowerCase();
  if (!text) return true;
  const { node } = entry;
  return [node.name, node.node_id, node.public_ipv4, node.public_ipv6, regionName(node.public_ipv4_country ?? '')]
    .filter(Boolean)
    .some(value => String(value).toLowerCase().includes(text));
}

/* ══ 页面 ══ */

const EMPTY_HINT = '先登记机器身份与网络信息，再在目标机器上安装 Agent。';

export function MockNodePage({ option }: { option: Option; scene: SceneKey; machines: Machine[] }) {
  const fleet = useFleet();
  const { who } = useSession();
  const selection = useSelection();
  const [tab, setTab] = useState<Tab>('live');
  const [view, setView] = useState<'cards' | 'list'>('cards');
  const [group, setGroup] = useState('all');
  const [query, setQuery] = useState('');
  if (fleet.nodes.isPending) return <Loading variant="nodes" sheeted />;

  const changeTab = (next: Tab) => {
    selection.exit();
    setTab(next);
  };
  const empty = fleet.entries.length === 0 && (
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
      {EMPTY_HINT}
    </EmptyState>
  );
  const meta = fleet.live.length > 0 ? `${fleet.live.length} 台 · ${regionCount(fleet.live)} 个地区` : null;

  if (option === 'd') {
    const groups = railGroups(fleet);
    const chosen = [...groups.status, ...groups.regions].find(item => item.key === group) ?? groups.status[0];
    const shown = chosen.entries.filter(entry => matches(entry, query));
    return (
      <div className="nd-sheet nd-page nl-page" data-option={option}>
        <div className="fg-sheet nd-paper">
          <header className="nd-page-head">
            <FleetIdentity fleet={fleet} meta={meta} />
            <label className="nl-search">
              <Icon of="search" size={13} />
              <input
                className="f"
                type="search"
                value={query}
                placeholder="名称、ID 或地址"
                aria-label="搜索机器"
                onChange={event => setQuery(event.target.value)}
              />
            </label>
            <FleetTools fleet={fleet} selection={selection} showRetired={chosen.key === 'retired'} />
          </header>
          <div className="nd-paper-body nl-rail-body">
            {empty || (
              <>
                <Rail
                  groups={groups}
                  value={chosen.key}
                  onChange={key => {
                    selection.exit();
                    setGroup(key);
                  }}
                />
                <div className="nl-rail-main">
                  {shown.length === 0 ? (
                    <p className="nl-empty">{query ? `没有匹配「${query}」的机器` : '这里没有机器'}</p>
                  ) : (
                    <CardGrid entries={shown} fleet={fleet} selection={selection} why={chosen.key === 'attention'} />
                  )}
                </div>
              </>
            )}
          </div>
        </div>
      </div>
    );
  }

  const shown = tab === 'live' ? fleet.live : tab === 'attention' ? fleet.attention : fleet.retired;
  const viewSwitch = option === 'c' && (
    <span className="segsw nl-view" role="group" aria-label="视图">
      {(
        [
          ['cards', '卡片'],
          ['list', '列表'],
        ] as const
      ).map(([value, label]) => (
        <button key={value} type="button" aria-pressed={view === value} title={label} onClick={() => setView(value)}>
          {value === 'cards' ? <GridIcon /> : <Icon of="menu" size={13} className="nd-tool-icon" />}
          <span className="nl-view-label">{label}</span>
        </button>
      ))}
    </span>
  );
  const body =
    empty ||
    (shown.length === 0 ? (
      <p className="nl-empty">{tab === 'attention' ? '没有需要处理的机器' : '没有在用机器'}</p>
    ) : option === 'b' ? (
      <RegionSections entries={shown} fleet={fleet} selection={selection} why={tab === 'attention'} />
    ) : option === 'c' && view === 'list' ? (
      <FleetTable entries={shown} fleet={fleet} selection={selection} />
    ) : (
      <CardGrid entries={shown} fleet={fleet} selection={selection} why={tab === 'attention'} />
    ));
  return (
    <div className="nd-sheet nd-page nl-page" data-option={option}>
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head">
          <FleetIdentity fleet={fleet} meta={meta} />
          {fleet.entries.length > 0 && <FleetTabs tab={tab} onTab={changeTab} fleet={fleet} />}
          <FleetTools fleet={fleet} selection={selection} showRetired={tab === 'retired'} before={viewSwitch} />
        </header>
        <div className="nd-paper-body">{body}</div>
      </div>
    </div>
  );
}
