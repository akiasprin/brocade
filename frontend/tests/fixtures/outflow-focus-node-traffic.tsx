// Mockup fork of src/node-traffic.tsx for outflow-focus-mockup.html; never imported by the app.
// Change against production: clicking a user's flow in 流入统计 narrows 流出统计 to that user, and a click on blank
// space restores the machine view. The user's name still opens 流入明细.
import { useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  fetchNodeInflowUsage,
  fetchSnapshot,
  type ConsoleSnapshot,
  type NodeInflowRow,
  type NodeInflowUsage,
  type SiteUsagePeriod,
  type TrafficTimeRange,
} from '../../src/api';
import { NodeInflowDetailsDialog } from '../../src/node-inflow-details';
import { parseTrafficRange, trafficDateInput, trafficRangeText } from '../../src/node-traffic-range';
import { fallbackSource, NodeUserTrafficCard, type TrafficSource } from '../../src/user-traffic-realtime';
import { NodeRealtimeProvider } from '../../src/node-realtime';
import { NodeSiteUsageCard, type FocusOption, type FocusUser } from './outflow-focus-site-usage';
import { ErrorBox, SegmentedControl } from '../../src/ui/bits';
import { DialogClose, DialogLayer } from '../../src/ui/dialog';
import { bytes, shareOf } from '../../src/ui/format';
import { Icon, PanelTitle } from '../../src/ui/icons';
import { PanelLoading } from '../../src/ui/loading';
import { useMetricQuery } from '../../src/ui/metric-cadence';
import { ObserveBrushProvider } from '../../src/ui/observe-brush';
import { RegionFlag } from '../../src/ui/region-flag';
import { GeneratedUserAvatar } from '../../src/ui/user-avatar';
import '../../src/node-traffic.css';

const PERIODS = [
  { value: 'day', label: '当天' },
  { value: 'week', label: '当周' },
  { value: 'month', label: '当月' },
  { value: 'custom', label: '自定义' },
] as const;
const COLORS = ['--observe-1', '--observe-6', '--observe-4', '--observe-5', '--observe-7'];
const FOLD = 12;
const sourceId = (row: NodeInflowRow) =>
  row.source === 'chain' ? JSON.stringify([row.app_id, row.chain_id, row.ingress_id]) : row.source;

/** Names come from the current model; attribution always comes from the recorded row. */
export function trafficSources(snapshot: ConsoleSnapshot | undefined) {
  const sources = new Map<string, TrafficSource>();
  for (const app of snapshot?.snapshot.apps ?? []) {
    for (const chain of app.chains) {
      const source: TrafficSource = {
        key: JSON.stringify([app.id, chain.id, null]),
        name: chain.name || chain.id,
        country: chain.subscription_country ?? null,
      };
      sources.set(source.key, source);
      for (const ingress of app.ingresses.filter(ingress => ingress.chain === chain.id))
        sources.set(`ingress:${ingress.id}`, source);
    }
  }
  return (key: string): TrafficSource => sources.get(key) ?? fallbackSource(key);
}
/** Keep authorized idle users in the summary without duplicating users with several ingress grants. */
export function trafficUserKeys(snapshot: ConsoleSnapshot | undefined, nodeId: string): string[] {
  return [
    ...new Set(
      (snapshot?.snapshot.apps ?? []).flatMap(app => {
        const ingresses = new Set(app.ingresses.filter(ingress => ingress.node === nodeId).map(ingress => ingress.id));
        return app.grants.filter(grant => ingresses.has(grant.ingress)).map(grant => `${grant.tenant}/${grant.user}`);
      }),
    ),
  ];
}
interface Cell {
  total: number;
  down: number;
  up: number;
  gap: boolean;
}
interface InflowUser extends Cell {
  id: string;
  tenantId: string;
  name: string;
  cells: Map<string, Cell>;
}
interface InflowSource extends TrafficSource {
  id: string;
  color: string;
  total: number;
  kind: NodeInflowRow['source'];
}

function inflowModel(data: NodeInflowUsage, sourceOf: (key: string) => TrafficSource) {
  const users = new Map<string, InflowUser>();
  const sources = new Map<string, InflowSource>();
  for (const row of data.rows) {
    const id = sourceId(row);
    const total = row.downlink_bytes + row.uplink_bytes;
    if (total === 0) continue;
    const userId = `${row.tenant_id}/${row.user_id}`;
    const user = users.get(userId) ?? {
      id: userId,
      tenantId: row.tenant_id,
      name: row.user_id,
      total: 0,
      down: 0,
      up: 0,
      gap: false,
      cells: new Map<string, Cell>(),
    };
    const cell = user.cells.get(id) ?? { total: 0, down: 0, up: 0, gap: false };
    cell.total += total;
    cell.down += row.downlink_bytes;
    cell.up += row.uplink_bytes;
    cell.gap ||= row.has_gap;
    user.total += total;
    user.down += row.downlink_bytes;
    user.up += row.uplink_bytes;
    user.gap ||= row.has_gap;
    user.cells.set(id, cell);
    users.set(userId, user);
    const known = sourceOf(id);
    const name =
      row.source === 'forwarding'
        ? '端口转发'
        : row.source === 'unattributed'
          ? '未按接入链记录'
          : !known.muted
            ? known.name
            : row.chain_id
              ? `接入链 ${row.chain_id}`
              : `历史接入点 ${row.ingress_id ?? ''}`;
    const source = sources.get(id) ?? { ...known, id, key: id, name, kind: row.source, total: 0, color: '' };
    source.total += total;
    sources.set(id, source);
  }
  const rows = [...users.values()].sort((a, b) => b.total - a.total || a.id.localeCompare(b.id));
  const views = [...sources.values()].sort((a, b) => b.total - a.total || a.id.localeCompare(b.id));
  return { rows, views, grand: rows.reduce((sum, row) => sum + row.total, 0) };
}
function ChainName({ view }: { view: InflowSource }) {
  return (
    <span className="nt-chain">
      {view.kind === 'forwarding' ? (
        <Icon of="forwarding" size={13} className="traffic-source-icon" />
      ) : (
        view.country && <RegionFlag code={view.country} />
      )}
      <span className="nt-chain-name">{view.name}</span>
    </span>
  );
}
function Swatch({ view }: { view: InflowSource }) {
  return <i className="nt-swatch" style={{ background: view.color }} aria-hidden="true" />;
}
function Who({ row }: { row: InflowUser }) {
  return (
    <span className="nti-who" title={row.name}>
      <GeneratedUserAvatar id={row.name} variant="mini" />
      <span className="nti-name">{row.name}</span>
    </span>
  );
}
const cellText = (row: InflowUser, view: InflowSource) => {
  const cell = row.cells.get(view.id);
  return `${row.name} → ${view.name} · ${bytes(cell?.total ?? 0)} · 下行 ${bytes(cell?.down ?? 0)} · 上行 ${bytes(cell?.up ?? 0)}${cell?.gap ? ' · 采集有缺口，仅含已记录的流量' : ''}`;
};
type BodyProps = ReturnType<typeof inflowModel> & {
  onUserSelect: (row: InflowUser) => void;
  /** The user 流出统计 is narrowed to. */
  focus: string | null;
  onFocus: (row: InflowUser | null) => void;
};
/** A flow, a bar or a node selects its user; pressing it again restores the machine view. */
const focusKeys = (toggle: () => void) => (event: KeyboardEvent) => {
  if (event.key !== 'Enter' && event.key !== ' ') return;
  event.preventDefault();
  toggle();
};
function BarsBody({ rows, views, grand, onUserSelect, focus: focusUser, onFocus }: BodyProps) {
  const [focus, setFocus] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const ordered = focus
    ? [...views.filter(view => view.id === focus), ...views.filter(view => view.id !== focus)]
    : views;
  return (
    <>
      <div className="nti-legend" role="group" aria-label="接入链（点击聚焦）">
        {views.map(view => (
          <button
            key={view.id}
            type="button"
            className={`nti-key${focus === view.id ? ' on' : ''}`}
            aria-pressed={focus === view.id}
            onClick={() => setFocus(focus === view.id ? null : view.id)}
          >
            <Swatch view={view} />
            <ChainName view={view} />
            <b>{bytes(view.total)}</b>
            <span className="nti-share">{shareOf(view.total, grand)}</span>
          </button>
        ))}
      </div>
      {/* 整行（条形与数值）聚焦用户，名字打开流入明细。 */}
      <ol className="nti-bars">
        {(expanded ? rows : rows.slice(0, FOLD)).map(row => {
          const pinned = focusUser === row.id;
          const toggle = () => onFocus(pinned ? null : row);
          return (
            // 条形与数值盖在整行按钮上方，保留各链段的悬停说明；点到它们同样选中用户。
            <li
              key={row.id}
              className={`nti-bar-row ofm-row${pinned ? ' pinned' : focusUser ? ' unpinned' : ''}`}
              onClick={event => {
                if (!(event.target as Element).closest('button')) toggle();
              }}
            >
              <button
                type="button"
                className="ofm-hit"
                aria-pressed={pinned}
                aria-label={`只看 ${row.name} 的流出`}
                onClick={toggle}
              />
              <button
                type="button"
                className="ofm-who"
                aria-label={`${row.name} · ${bytes(row.total)} · 流入明细`}
                aria-haspopup="dialog"
                onClick={() => onUserSelect(row)}
              >
                <Who row={row} />
              </button>
              <span className="nti-track">
                <span className="nti-bar" style={{ width: `${(row.total / rows[0].total) * 100}%` }}>
                  {ordered.map(view => {
                    const cell = row.cells.get(view.id);
                    return (
                      cell && (
                        <i
                          key={view.id}
                          title={cellText(row, view)}
                          className={focus && focus !== view.id ? 'dim' : ''}
                          style={{ flexGrow: cell.total, background: view.color }}
                        />
                      )
                    );
                  })}
                </span>
              </span>
              <span className="nti-num">{bytes(row.total)}</span>
              <span className="nti-pct">{shareOf(row.total, grand)}</span>
            </li>
          );
        })}
      </ol>
      {rows.length > FOLD && (
        <button
          type="button"
          className="btn sm nti-more"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? '收起' : `展开其余 ${rows.length - FOLD} 位用户`}
        </button>
      )}
    </>
  );
}
function FlowBody({ rows, views, grand, onUserSelect, focus, onFocus }: BodyProps) {
  const box = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [narrow, setNarrow] = useState(true);
  const [hot, setHot] = useState<{ user: string | null; col: string | null } | null>(null);
  const [allUsers, setAllUsers] = useState(false);
  useLayoutEffect(() => {
    const element = box.current;
    if (!element) return;
    const measure = () => {
      setWidth(element.getBoundingClientRect().width);
      setNarrow(
        (element.closest('.nti-card')?.getBoundingClientRect().width ?? element.getBoundingClientRect().width) < 720,
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const rest = allUsers ? [] : rows.slice(10);
  const restRow: InflowUser = {
    id: 'rest',
    tenantId: '',
    name: `其余 ${rest.length} 位`,
    down: 0,
    up: 0,
    total: 0,
    gap: false,
    cells: new Map(),
  };
  for (const row of rest) {
    restRow.total += row.total;
    restRow.down += row.down;
    restRow.up += row.up;
    restRow.gap ||= row.gap;
    for (const [key, cell] of row.cells) {
      const prior = restRow.cells.get(key) ?? { total: 0, down: 0, up: 0, gap: false };
      restRow.cells.set(key, {
        total: prior.total + cell.total,
        down: prior.down + cell.down,
        up: prior.up + cell.up,
        gap: prior.gap || cell.gap,
      });
    }
  }
  const left = rest.length > 0 ? [...rows.slice(0, 10), restRow] : rows;
  const labelLeft = 186,
    labelRight = 250,
    node = 10,
    gap = 6;
  const plot = Math.max(120, width - labelLeft - labelRight);
  const scale = (Math.max(200, Math.max(left.length, views.length) * 28) * 0.72) / grand;
  const layout = (values: number[]) => values.map(value => Math.max(value * scale, 24));
  const lslots = layout(left.map(row => row.total)),
    rslots = layout(views.map(view => view.total));
  const sum = (slots: number[]) => slots.reduce((a, b) => a + b, 0) + gap * (slots.length - 1);
  const height = Math.max(sum(lslots), sum(rslots));
  const positions = (values: number[], slots: number[]) => {
    let cursor = (height - sum(slots)) / 2;
    return values.map((value, i) => {
      const h = value * scale;
      const out = { y: cursor + (slots[i] - h) / 2, h };
      cursor += slots[i] + gap;
      return out;
    });
  };
  const lp = positions(
      left.map(row => row.total),
      lslots,
    ),
    rp = positions(
      views.map(view => view.total),
      rslots,
    );
  const usedLeft = left.map(() => 0),
    usedRight = views.map(() => 0),
    mid = plot / 2;
  const links = left.flatMap((row, i) =>
    views.flatMap((view, j) => {
      const value = row.cells.get(view.id)?.total ?? 0;
      if (value <= 0) return [];
      const h = value * scale,
        y0 = lp[i].y + usedLeft[i],
        y1 = rp[j].y + usedRight[j];
      usedLeft[i] += h;
      usedRight[j] += h;
      return [
        {
          row,
          view,
          d: `M${node} ${y0}C${mid} ${y0} ${mid} ${y1} ${plot - node} ${y1}L${plot - node} ${y1 + h}C${mid} ${y1 + h} ${mid} ${y0 + h} ${node} ${y0 + h}Z`,
        },
      ];
    }),
  );
  // Hovering previews; otherwise the focused user stays lit. A lit user keeps its chains lit and a lit chain its users.
  const active = hot ?? (focus ? { user: focus, col: null } : null);
  const uses = (user: string, col: string) => !!left.find(row => row.id === user)?.cells.has(col);
  const lit = (on: boolean) => (!active ? '' : on ? ' on' : ' dim');
  const linkState = (user: string, col: string) => lit((active?.user ?? user) === user && (active?.col ?? col) === col);
  const userState = (user: string) =>
    lit(active?.user != null ? active.user === user : !!active?.col && uses(user, active.col));
  const chainState = (col: string) =>
    lit(active?.col != null ? active.col === col : !!active?.user && uses(active.user, col));
  return (
    <div ref={box}>
      {!narrow ? (
        <>
          <div className="ntf-box" style={{ height }} onMouseLeave={() => setHot(null)}>
            <div className="ntf-labels left" style={{ width: labelLeft }}>
              {left.map((row, i) => (
                <button
                  type="button"
                  key={row.id}
                  className={`ntf-label left${userState(row.id)}`}
                  aria-label={`${row.name} · ${bytes(row.total)}`}
                  style={{ top: lp[i].y + lp[i].h / 2 }}
                  title={`${row.name} · 下行 ${bytes(row.down)} · 上行 ${bytes(row.up)}`}
                  aria-haspopup={row.id === 'rest' ? undefined : 'dialog'}
                  onClick={() => (row.id === 'rest' ? setAllUsers(true) : onUserSelect(row))}
                  onMouseEnter={() => setHot({ user: row.id, col: null })}
                  onFocus={() => setHot({ user: row.id, col: null })}
                  onBlur={() => setHot(null)}
                >
                  <span className="ntf-name">{row.name}</span>
                  <b>{bytes(row.total)}</b>
                </button>
              ))}
            </div>
            <svg
              className="ntf-svg"
              width={plot}
              height={height}
              style={{ left: labelLeft }}
              role="group"
              aria-label="用户到接入链的流向，带宽与流量成正比"
            >
              {/* 一位用户的流向与节点是一个按钮：聚焦后流出统计只看这位用户，「其余」则展开全部用户。 */}
              {left.map((row, i) => {
                const toggle = () => (row.id === 'rest' ? setAllUsers(true) : onFocus(focus === row.id ? null : row));
                return (
                  <g
                    key={row.id}
                    className="ofm-flow"
                    role="button"
                    tabIndex={0}
                    aria-pressed={row.id === 'rest' ? undefined : focus === row.id}
                    aria-label={row.id === 'rest' ? `展开其余 ${rest.length} 位用户` : `只看 ${row.name} 的流出`}
                    onClick={toggle}
                    onKeyDown={focusKeys(toggle)}
                  >
                    {links
                      .filter(link => link.row === row)
                      .map(({ view, d }) => (
                        <path
                          key={view.id}
                          className={`ntf-link${linkState(row.id, view.id)}`}
                          d={d}
                          style={{ fill: view.color }}
                          onMouseEnter={() => setHot({ user: row.id, col: view.id })}
                        >
                          <title>{cellText(row, view)}</title>
                        </path>
                      ))}
                    <rect
                      className={`ntf-node${userState(row.id)}`}
                      x={0}
                      y={lp[i].y}
                      width={node}
                      height={lp[i].h}
                      rx={2}
                      onMouseEnter={() => setHot({ user: row.id, col: null })}
                    />
                  </g>
                );
              })}
              {views.map((view, i) => (
                <rect
                  key={view.id}
                  className={`ntf-node${chainState(view.id)}`}
                  x={plot - node}
                  y={rp[i].y}
                  width={node}
                  height={rp[i].h}
                  rx={2}
                  style={{ fill: view.color }}
                />
              ))}
            </svg>
            <div className="ntf-labels right" style={{ width: labelRight }}>
              {views.map((view, i) => (
                <button
                  type="button"
                  key={view.id}
                  className={`ntf-label right${chainState(view.id)}`}
                  style={{ top: rp[i].y + rp[i].h / 2 }}
                  aria-label={`${view.name} · ${bytes(view.total)}`}
                  onClick={() =>
                    setHot(hot?.col === view.id && hot.user === null ? null : { user: null, col: view.id })
                  }
                  onMouseEnter={() => setHot({ user: null, col: view.id })}
                  onFocus={() => setHot({ user: null, col: view.id })}
                  onBlur={() => setHot(null)}
                >
                  <ChainName view={view} />
                  <b>{bytes(view.total)}</b>
                  <span className="nti-share">{shareOf(view.total, grand)}</span>
                </button>
              ))}
            </div>
          </div>

          {rows.length > 10 && (
            <button
              type="button"
              className="btn sm nti-more"
              aria-expanded={allUsers}
              onClick={() => setAllUsers(!allUsers)}
            >
              {allUsers ? '收起用户' : `展开全部 ${rows.length} 位用户`}
            </button>
          )}
        </>
      ) : (
        <BarsBody rows={rows} views={views} grand={grand} onUserSelect={onUserSelect} focus={focus} onFocus={onFocus} />
      )}
    </div>
  );
}

export function NodeInflowUsageCard({
  nodeId,
  nodeName,
  period,
  sourceOf = fallbackSource,
  range,
  focus = null,
  onFocus = () => {},
}: {
  nodeId: string;
  nodeName?: string;
  period: SiteUsagePeriod;
  sourceOf?: (key: string) => TrafficSource;
  range?: TrafficTimeRange;
  focus?: string | null;
  onFocus?: (user: FocusUser | null) => void;
}) {
  const [selected, setSelected] = useState<InflowUser | null>(null);
  const rangeArgs = range ? ([range] as const) : ([] as const);
  const usage = useMetricQuery({
    queryKey: ['node-inflow-usage', nodeId, period, ...rangeArgs],
    queryFn: () => fetchNodeInflowUsage(nodeId, period, ...rangeArgs),
    refetchInterval: 30_000,
    staleTime: 15_000,
  });
  const month = useMetricQuery({
    queryKey: ['node-inflow-usage', nodeId, 'month'],
    queryFn: () => fetchNodeInflowUsage(nodeId, 'month'),
    refetchInterval: 30_000,
    staleTime: 15_000,
  });
  const model = useMemo(() => (usage.data ? inflowModel(usage.data, sourceOf) : null), [usage.data, sourceOf]);
  const monthOrder = useMemo(
    () =>
      month.data
        ? inflowModel(month.data, sourceOf)
            .views.filter(view => view.kind === 'chain')
            .map(view => view.id)
        : [],
    [month.data, sourceOf],
  );
  const paletteScope = `${nodeId}/${month.data?.range_start_unix_secs ?? ''}`;
  const [assignment, setAssignment] = useState<{ scope: string; ids: string[] }>({ scope: '', ids: [] });
  const newIds = assignment.scope === paletteScope ? monthOrder.filter(id => !assignment.ids.includes(id)) : monthOrder;
  const palette = assignment.scope === paletteScope ? assignment.ids : monthOrder;
  if (month.data && (assignment.scope !== paletteScope || newIds.length > 0))
    setAssignment({
      scope: paletteScope,
      ids: assignment.scope === paletteScope ? [...assignment.ids, ...newIds] : monthOrder,
    });
  const views = model?.views.map(view => ({
    ...view,
    color:
      view.kind === 'forwarding'
        ? 'color-mix(in srgb, var(--ink) 40%, transparent)'
        : view.kind === 'unattributed'
          ? 'color-mix(in srgb, var(--ink) 20%, transparent)'
          : `var(${COLORS[palette.indexOf(view.id)] ?? '--observe-10'})`,
  }));
  return (
    <section className="panel config-panel nti-card" aria-label="流入统计">
      <header>
        <PanelTitle of="usage">流入统计</PanelTitle>
        <span className="site-usage-note">用户 → 接入链 · 含端口转发</span>
      </header>
      <div className="nti-body panel-full-bleed-body" aria-busy={usage.isFetching}>
        {usage.isPending && <p role="status">正在读取流入统计…</p>}
        {usage.isError && (
          <p role="alert">
            流入统计{usage.data ? '刷新失败，当前显示上次读取的数据。' : '读取失败。'}
            <button type="button" className="btn text sm" onClick={() => usage.refetch()}>
              重试
            </button>
          </p>
        )}
        {month.isError && (
          <p role="alert">
            本月接入链颜色参考读取失败。
            <button type="button" className="btn text sm" onClick={() => month.refetch()}>
              重试
            </button>
          </p>
        )}
        {model && views && (
          <>
            <div className="site-usage-head nti-head">
              <span className="site-usage-total">{bytes(model.grand)}</span>
              <span className="site-usage-label">
                {model.rows.length} 位用户 · {views.length} 个来源
              </span>
              <span className="site-usage-meta">
                下行 {bytes(model.rows.reduce((sum, row) => sum + row.down, 0))} · 上行{' '}
                {bytes(model.rows.reduce((sum, row) => sum + row.up, 0))}
              </span>
            </div>
            {model.grand > 0 ? (
              <FlowBody
                key={`${nodeId}/${period}`}
                rows={model.rows}
                views={views}
                grand={model.grand}
                onUserSelect={setSelected}
                focus={focus}
                onFocus={row => onFocus(row && { id: row.id, tenantId: row.tenantId, userId: row.name })}
              />
            ) : (
              <p className="site-usage-state">
                {usage.data?.latest_report_unix_secs === null
                  ? '本机尚无流量上报，不能视为零流量。'
                  : '所选范围没有已记录的流入流量。'}
              </p>
            )}
            {model.rows.some(row => row.gap) && <p className="site-usage-note warn">采集有缺口，仅含已记录的流量。</p>}
            {usage.data?.range_limited && (
              <p className="site-usage-note warn">
                所选时间段超出窗口明细保留范围，统计仅含尚存明细，不能视为完整时段总量。
              </p>
            )}
            {views.some(view => view.kind === 'unattributed') && (
              <p className="site-usage-note">「未按接入链记录」是按链统计上线前的用户合计，不估算、不均摊到现有链。</p>
            )}
            {!!usage.data?.idle_users.length && (
              <p className="site-usage-note">另有 {usage.data.idle_users.length} 位已授权用户在本范围无流入记录。</p>
            )}
          </>
        )}
        <p className="site-usage-note">
          只统计本机认证接入与端口转发，中继与落地流量不含用户身份；仅含已记录流量，不表示全时段采集完整。
        </p>
        {range && (
          <p className="site-usage-note">按采样窗口起点计入所选时段，通常每 30 秒一个窗口；不按时长估算窗口内流量。</p>
        )}
      </div>
      {selected && (
        <NodeInflowDetailsDialog
          key={`${nodeId}/${selected.id}`}
          nodeId={nodeId}
          nodeName={nodeName}
          tenantId={selected.tenantId}
          userId={selected.name}
          period={period}
          range={range}
          sourceOf={sourceOf}
          onClose={() => setSelected(null)}
        />
      )}
    </section>
  );
}
const KEEP_FOCUS =
  'button, a, input, select, textarea, summary, label, [role="button"], canvas, .site-usage-chart, .ofm-row';
/** Only a click on blank space restores the machine view: not on a control, a chart or the end of a text selection. */
function blankClick(event: MouseEvent<HTMLElement>) {
  if (!(window.getSelection()?.isCollapsed ?? true)) return false;
  for (
    let element = event.target as Element | null;
    element && element !== event.currentTarget;
    element = element.parentElement
  )
    if (element.matches(KEEP_FOCUS)) return false;
  return true;
}
export function NodeTrafficDetails({
  nodeId,
  nodeName,
  snapshot,
  period,
  range,
  onRangeChange,
  option = 'a',
  initialFocus = null,
}: {
  nodeId: string;
  nodeName?: string;
  snapshot?: ConsoleSnapshot;
  period: SiteUsagePeriod;
  range?: TrafficTimeRange;
  onRangeChange?: (range: TrafficTimeRange | undefined) => void;
  option?: FocusOption;
  initialFocus?: FocusUser | null;
}) {
  const sourceOf = useMemo(() => trafficSources(snapshot), [snapshot]);
  const targets = useMemo(() => trafficUserKeys(snapshot, nodeId), [snapshot, nodeId]);
  // The focus survives period and range changes; only a blank click or the chip clears it.
  const [focus, setFocus] = useState<FocusUser | null>(initialFocus);
  return (
    <ObserveBrushProvider
      rangeKey={JSON.stringify(range)}
      pending={false}
      onSelect={(start, end) => {
        if (!onRangeChange) return false;
        const from = Math.floor(start / 30) * 30;
        const to = Math.min(Math.ceil(end / 30) * 30, Math.floor(Date.now() / 30_000) * 30);
        if (from >= to) return false;
        onRangeChange({ start_unix_secs: from, end_unix_secs: to });
        return true;
      }}
      onZoomOut={() => onRangeChange?.(undefined)}
    >
      <div
        className="nt-details"
        onClick={event => {
          if (focus && blankClick(event)) setFocus(null);
        }}
      >
        <NodeUserTrafficCard nodeId={nodeId} sourceOf={sourceOf} targets={targets} period={period} range={range} />
        <NodeInflowUsageCard
          nodeId={nodeId}
          nodeName={nodeName}
          period={period}
          sourceOf={sourceOf}
          range={range}
          focus={focus?.id ?? null}
          onFocus={setFocus}
        />
        <NodeSiteUsageCard
          nodeId={nodeId}
          period={period}
          range={range}
          focus={focus}
          onClearFocus={() => setFocus(null)}
          option={option}
        />
      </div>
    </ObserveBrushProvider>
  );
}

/** Shared machine usage overlay; the model and live subscription are acquired only when opened. */
export function NodeTrafficDialog({
  nodeId,
  name,
  country,
  initialPeriod = 'day',
  onClose,
  option,
  initialFocus,
}: {
  nodeId: string;
  name: string;
  country: string | null;
  initialPeriod?: SiteUsagePeriod;
  onClose: () => void;
  option?: FocusOption;
  initialFocus?: FocusUser | null;
}) {
  const titleId = useId();
  const [period, setPeriod] = useState<SiteUsagePeriod>(initialPeriod);
  const [range, setRange] = useState<TrafficTimeRange>();
  const [editing, setEditing] = useState(false);
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [rangeError, setRangeError] = useState('');
  const changeRange = (next: TrafficTimeRange | undefined) => {
    setRange(next);
    setEditing(false);
    setRangeError('');
    if (next) {
      setPeriod('day');
      setStart(trafficDateInput(next.start_unix_secs));
      setEnd(trafficDateInput(next.end_unix_secs));
    }
  };
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot(), staleTime: 30_000 });
  return (
    <DialogLayer label={`${name} · 用量`} onClose={onClose}>
      <section className="dialog-surface node-usage-dialog" aria-labelledby={titleId}>
        <header className="node-usage-header">
          <div className="node-usage-identity">
            {country && <RegionFlag code={country} square />}
            <h2 id={titleId} title={name}>
              {name}
            </h2>
          </div>
          <div className="node-usage-actions">
            <div className="node-usage-period" title="统计区间（UTC+8）· 适用于历史流量、流入与流出统计">
              <span className="node-usage-period-label">统计</span>
              <SegmentedControl
                ariaLabel="流量统计范围"
                value={editing || range ? 'custom' : period}
                options={PERIODS}
                onChange={value => {
                  if (value === 'custom') {
                    const until = Math.floor(Date.now() / 30_000) * 30;
                    if (!range) {
                      setStart(trafficDateInput(until - 15 * 60));
                      setEnd(trafficDateInput(until));
                    }
                    setEditing(true);
                  } else {
                    setPeriod(value);
                    changeRange(undefined);
                  }
                }}
              />
            </div>
            <DialogClose className="btn sm node-usage-close" aria-label="关闭机器用量" title="关闭">
              <Icon of="close" size={16} />
            </DialogClose>
          </div>
        </header>
        {editing && (
          <form
            className="nt-range-editor"
            onSubmit={event => {
              event.preventDefault();
              const parsed = parseTrafficRange(start, end);
              if (typeof parsed === 'string') {
                setRangeError(parsed);
                return;
              }
              changeRange(parsed);
            }}
          >
            <label>
              开始时间（UTC+8）
              <input
                type="datetime-local"
                step={30}
                required
                value={start}
                onChange={event => setStart(event.target.value)}
              />
            </label>
            <label>
              结束时间（UTC+8）
              <input
                type="datetime-local"
                step={30}
                required
                value={end}
                onChange={event => setEnd(event.target.value)}
              />
            </label>
            <button type="submit" className="btn sm primary">
              应用
            </button>
            <button
              type="button"
              className="btn text sm"
              onClick={() => {
                setEditing(false);
                setRangeError('');
              }}
            >
              取消
            </button>
            {rangeError && <p role="alert">{rangeError}</p>}
          </form>
        )}
        {range && (
          <div className="nt-selected-range">
            <Icon of="clock" size={14} />
            <span>{trafficRangeText(range.start_unix_secs, range.end_unix_secs)} · UTC+8</span>
          </div>
        )}
        <div
          className="node-usage-body"
          tabIndex={0}
          aria-label="机器用量内容"
          aria-busy={snapshot.isPending || undefined}
        >
          {snapshot.error && <ErrorBox error={snapshot.error} />}
          {snapshot.isPending && <PanelLoading label="加载机器用量" />}
          {snapshot.data && (
            <NodeRealtimeProvider key={nodeId} nodeId={nodeId}>
              <NodeTrafficDetails
                nodeId={nodeId}
                nodeName={name}
                snapshot={snapshot.data}
                period={period}
                range={range}
                onRangeChange={changeRange}
                option={option}
                initialFocus={initialFocus}
              />
            </NodeRealtimeProvider>
          )}
        </div>
      </section>
    </DialogLayer>
  );
}
