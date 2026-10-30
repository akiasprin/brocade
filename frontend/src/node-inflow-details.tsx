import { useId, useMemo, useState } from 'react';
import {
  fetchNodeInflowDetails,
  type NetworkOperator,
  type NodeInflowDetailRow,
  type NodeInflowDetails,
  type SiteUsagePeriod,
  type TrafficTimeRange,
} from './api';
import { fallbackSource, type TrafficSource } from './user-traffic-realtime';
import { inflowSourceKey } from './node-inflow-source';
import { SegmentedControl } from './ui/bits';
import { CopyButton } from './ui/copy-button';
import { DialogClose, DialogLayer } from './ui/dialog';
import { bytes, shareOf } from './ui/format';
import { Icon } from './ui/icons';
import { PanelLoading } from './ui/loading';
import { useMetricQuery } from './ui/metric-cadence';
import { RegionFlag } from './ui/region-flag';
import { SOURCE_LOCATION_TITLE, SourceAddress, sourceNetwork } from './ui/source-address';
import { GeneratedUserAvatar } from './ui/user-avatar';
import './node-traffic.css';

/* 机器用量 → 流入统计 → 点用户：这位用户在本机的来源排行。每个来源地址一行，跨链使用的地址
   只出现一次，条形按接入链分段着色；未记录来源、端口转发与未按接入链记录的部分始终列在最后。 */

type Metric = 'total' | 'down' | 'up';
interface Bytes {
  down: number;
  up: number;
}
const valueOf = (item: Bytes, metric: Metric) =>
  metric === 'down' ? item.down : metric === 'up' ? item.up : item.down + item.up;

interface Chain extends Bytes {
  key: string;
  kind: NodeInflowDetailRow['source'];
  name: string;
  country: string | null;
  color: string;
}
type SourceKind = 'ip' | 'residual' | 'forwarding' | 'unattributed';
interface Source extends Bytes {
  key: string;
  kind: SourceKind;
  ip: string | null;
  /** 只读视角的地址在服务端出口脱敏为 `203.0.***.***`，复制它没有意义。 */
  masked: boolean;
  country?: string;
  operator?: NetworkOperator;
  /** 按接入链拆开的流量。 */
  parts: Map<string, Bytes>;
}
interface Model extends Bytes {
  chains: Chain[];
  sources: Source[];
  /** 脱敏时同形的地址数不清，不写来源数。 */
  ips: number | null;
  gap: boolean;
}

const MASKED = '***';
const FORWARDING_GRAY = 'var(--nid-gray-1)';
const UNKNOWN_GRAY = 'var(--nid-gray-2)';
function chainName(row: NodeInflowDetailRow, source: TrafficSource) {
  if (row.source === 'forwarding') return '端口转发';
  if (row.source === 'unattributed') return '未按接入链记录';
  if (!source.muted) return source.name;
  return row.chain_id ? `接入链 ${row.chain_id}` : `历史接入点 ${row.ingress_id ?? ''}`;
}

/** 脱敏后不同地址可能同形：同一个掩码出现不止一次时对不上号，不取它的地区。 */
function byAddress<T>(entries: readonly (readonly [string, T])[]) {
  const map = new Map<string, T | null>();
  for (const [ip, value] of entries) map.set(ip, map.has(ip) ? null : value);
  return map;
}

function modelOf(
  data: NodeInflowDetails,
  sourceOf: (key: string) => TrafficSource,
  colorOf: (key: string) => string | undefined,
): Model {
  const countries = byAddress(data.source_countries?.map(entry => [entry.ip, entry.country] as const) ?? []);
  const operators = byAddress(data.source_operators?.map(entry => [entry.ip, entry.operator] as const) ?? []);
  const rowsPerAddress = byAddress(data.rows.flatMap(row => (row.source_ip ? [[row.source_ip, true] as const] : [])));
  const chains = new Map<string, Chain>();
  const sources = new Map<string, Source>();
  data.rows.forEach((row, index) => {
    const key = inflowSourceKey(row);
    const chain = chains.get(key) ?? {
      key,
      kind: row.source,
      name: chainName(row, sourceOf(key)),
      country: row.source === 'chain' ? (sourceOf(key).country ?? null) : null,
      color:
        row.source === 'chain'
          ? (colorOf(key) ?? 'var(--observe-10)')
          : row.source === 'forwarding'
            ? FORWARDING_GRAY
            : UNKNOWN_GRAY,
      down: 0,
      up: 0,
    };
    chain.down += row.downlink_bytes;
    chain.up += row.uplink_bytes;
    chains.set(key, chain);
    const ip = row.source_ip;
    const masked = !!ip && ip.includes(MASKED);
    const kind: SourceKind = ip ? 'ip' : row.source === 'chain' ? 'residual' : row.source;
    // 同一地址在几条链上合为一行；脱敏的地址可能是不同来源，逐行保留。
    const id = !ip ? kind : masked ? `${index}:${ip}` : ip;
    const located = !!ip && (!masked || rowsPerAddress.get(ip) !== null);
    const source = sources.get(id) ?? {
      key: id,
      kind,
      ip,
      masked,
      country: located ? (countries.get(ip!) ?? undefined) : undefined,
      operator: located ? (operators.get(ip!) ?? undefined) : undefined,
      down: 0,
      up: 0,
      parts: new Map<string, Bytes>(),
    };
    source.down += row.downlink_bytes;
    source.up += row.uplink_bytes;
    const part = source.parts.get(key) ?? { down: 0, up: 0 };
    part.down += row.downlink_bytes;
    part.up += row.uplink_bytes;
    source.parts.set(key, part);
    sources.set(id, source);
  });
  const list = [...chains.values()];
  const all = [...sources.values()];
  return {
    chains: list,
    sources: all,
    down: list.reduce((sum, chain) => sum + chain.down, 0),
    up: list.reduce((sum, chain) => sum + chain.up, 0),
    ips: all.some(source => source.masked) ? null : all.filter(source => source.kind === 'ip').length,
    gap: data.rows.some(row => row.has_gap),
  };
}

/* ── 读数栏 ── */

const PERIOD_WORDS: Record<SiteUsagePeriod, string> = { day: '当天', week: '当周', month: '当月' };
/** 统计口径与时段：「当天 · 10-18 00:00–21:40」。当天等口径止于查询时刻，只写到分；自定义时段对齐 30 秒，有秒时写出秒。 */
function rangeLabel(period: SiteUsagePeriod, range: TrafficTimeRange | undefined, start: number, end: number) {
  const parts = (seconds: number) => {
    const wall = new Date((seconds + 8 * 3600) * 1000);
    const two = (value: number) => String(value).padStart(2, '0');
    const second = range ? wall.getUTCSeconds() : 0;
    return {
      day: `${two(wall.getUTCMonth() + 1)}-${two(wall.getUTCDate())}`,
      time: `${two(wall.getUTCHours())}:${two(wall.getUTCMinutes())}${second ? `:${two(second)}` : ''}`,
    };
  };
  const from = parts(start);
  const to = parts(end);
  const span =
    from.day === to.day ? `${from.day} ${from.time}–${to.time}` : `${from.day} ${from.time} – ${to.day} ${to.time}`;
  return `${range ? '自定义' : PERIOD_WORDS[period]} · ${span}`;
}

function Readout({ model, label }: { model: Model; label: string }) {
  const total = model.down + model.up;
  const [figure, unit] = bytes(total).split(' ');
  return (
    <section className="nid-readout" aria-label="流入合计">
      <div className="nid-hero">
        <span className="nid-hero-label">{label} · UTC+8</span>
        <strong className="nid-hero-value">
          {figure} <small>{unit}</small>
        </strong>
      </div>
      <div className="nid-io">
        <div className="usage-split" aria-hidden="true">
          {total > 0 && (
            <>
              <i className="up" style={{ flexGrow: model.up }} />
              <i className="down" style={{ flexGrow: model.down }} />
            </>
          )}
        </div>
        <dl className="usage-io">
          <div className="up">
            <dt>
              <i aria-hidden="true" />
              上行
            </dt>
            <dd>{bytes(model.up)}</dd>
            <dd className="usage-share">{shareOf(model.up, total)}</dd>
          </div>
          <div className="down">
            <dt>
              <i aria-hidden="true" />
              下行
            </dt>
            <dd>{bytes(model.down)}</dd>
            <dd className="usage-share">{shareOf(model.down, total)}</dd>
          </div>
        </dl>
      </div>
      <dl className="nid-facts">
        {model.ips !== null && (
          <div>
            <dt>来源 IP</dt>
            <dd>{model.ips}</dd>
          </div>
        )}
        <div>
          <dt>接入链</dt>
          <dd>{model.chains.filter(chain => chain.kind === 'chain').length}</dd>
        </div>
      </dl>
    </section>
  );
}

/** 影响可信度的事实只在出现时写一行；与用量账本不一致时，账本值点 (!) 再看。 */
function Notices({ data, gap }: { data: NodeInflowDetails; gap: boolean }) {
  const [open, setOpen] = useState(false);
  const noteId = useId();
  const items = [data.limited && '来源记录不完整，仅含已记录部分', gap && '所选时段有采集缺口'].filter(
    (item): item is string => !!item,
  );
  if (items.length === 0 && !data.accounting_mismatch) return null;
  return (
    <div className="nid-notices">
      <p className="nid-notice" role="status">
        <Icon of="warn" size={13} />
        {items.join('；')}
        {data.accounting_mismatch && items.length > 0 && '；'}
        {data.accounting_mismatch && (
          <button
            type="button"
            className="nid-help"
            aria-expanded={open}
            aria-controls={noteId}
            onClick={() => setOpen(shown => !shown)}
          >
            与用量账本不一致
            <Icon of="info" size={13} />
          </button>
        )}
      </p>
      <p id={noteId} className="nid-note" hidden={!open}>
        用量账本：上行 {bytes(data.accounting_uplink_bytes)}、下行 {bytes(data.accounting_downlink_bytes)}
        。来源观测与账本的接入归属或采样窗口不同，无法确认接入链的部分合并为「未按接入链记录」，不重复计入各链。
      </p>
    </div>
  );
}

/* ── 来源排行 ── */

const METRICS = [
  { value: 'total' as const, label: '合计' },
  { value: 'down' as const, label: '下行' },
  { value: 'up' as const, label: '上行' },
];
/** 地址多时折叠，只多出一个时直接列出。 */
const FOLD = 10;
const SOURCE_LABELS: Record<Exclude<SourceKind, 'ip'>, string> = {
  residual: '未记录来源',
  forwarding: '端口转发 · 不区分来源',
  unattributed: '未按接入链记录',
};
const SOURCE_TITLES: Record<Exclude<SourceKind, 'ip'>, string> = {
  residual: '用量账本已计入、来源观测没有记录的部分：观测开启前、旧版 Xray 漏采或超出容量。不按 IP 均摊。',
  forwarding: '端口转发按规则记账，不记录来源 IP。',
  unattributed: '来源观测与用量账本的接入归属不一致，无法确认接入链的部分合并在这里。',
};

function ChainName({ chain }: { chain: Chain }) {
  return (
    <span className="nt-chain">
      {chain.kind === 'forwarding' ? (
        <Icon of="forwarding" size={13} className="traffic-source-icon" />
      ) : (
        chain.country && <RegionFlag code={chain.country} />
      )}
      <span className="nt-chain-name">{chain.name}</span>
    </span>
  );
}

function SourceRow({
  source,
  parts,
  value,
  max,
  whole,
  chainOf,
  metric,
}: {
  source: Source;
  parts: [string, Bytes][];
  value: number;
  max: number;
  whole: number;
  chainOf: Map<string, Chain>;
  metric: Metric;
}) {
  const network = sourceNetwork(source.country, source.operator);
  const split =
    parts.length > 1
      ? `${parts.map(([key, part]) => `${chainOf.get(key)!.name} ${bytes(valueOf(part, metric))}`).join(' · ')} · `
      : '';
  return (
    <li className={`nid-line ${source.kind}`} title={`${split}上行 ${bytes(source.up)} · 下行 ${bytes(source.down)}`}>
      {source.ip ? (
        <span className="nid-addr">
          <SourceAddress ip={source.ip} />
          {!source.masked && (
            <CopyButton
              className="user-fcopy"
              text={source.ip}
              label={`复制来源 IP ${source.ip}`}
              successLabel="来源 IP 已复制"
              failureLabel="来源 IP 复制失败"
              iconOnly
            />
          )}
        </span>
      ) : (
        <span className="nid-addr">
          <span className="nid-unknown" title={source.kind !== 'ip' ? SOURCE_TITLES[source.kind] : undefined}>
            {source.kind !== 'ip' && SOURCE_LABELS[source.kind]}
          </span>
        </span>
      )}
      {source.ip ? (
        <span className="nid-net" title={SOURCE_LOCATION_TITLE}>
          {network.code && <RegionFlag code={network.code} />}
          <span>{network.label}</span>
        </span>
      ) : (
        <span className="nid-net" />
      )}
      <span className="nid-track" aria-hidden="true">
        <span className="nid-meter" style={{ width: `${max > 0 ? Math.max((value / max) * 100, 0.6) : 0}%` }}>
          {parts.map(([key, part]) => (
            <i
              key={key}
              style={{
                flexGrow: valueOf(part, metric),
                background:
                  source.kind === 'ip'
                    ? chainOf.get(key)!.color
                    : source.kind === 'forwarding'
                      ? FORWARDING_GRAY
                      : UNKNOWN_GRAY,
              }}
            />
          ))}
        </span>
      </span>
      <span className="nid-num">{bytes(value)}</span>
      <span className="nid-pct">{shareOf(value, whole)}</span>
    </li>
  );
}

function SourceRanking({ model, metric }: { model: Model; metric: Metric }) {
  const [picked, setPicked] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const chainOf = new Map(model.chains.map(chain => [chain.key, chain]));
  // 刷新后不再出现的链不再筛选。
  const focus = picked && chainOf.has(picked) ? picked : null;
  const chains = [...model.chains].sort(
    (a, b) => valueOf(b, metric) - valueOf(a, metric) || a.key.localeCompare(b.key),
  );
  const whole = valueOf(model, metric);
  // 选中一条链时，每行只算它在这条链上的流量。
  const rows = model.sources
    .map(source => {
      const parts = [...source.parts].filter(([key, part]) => (!focus || key === focus) && valueOf(part, metric) > 0);
      return { source, parts, value: parts.reduce((sum, [, part]) => sum + valueOf(part, metric), 0) };
    })
    .filter(row => row.parts.length > 0)
    .sort((a, b) => b.value - a.value || a.source.key.localeCompare(b.source.key));
  // 只折叠地址；观测没能拆开的部分始终可见，排在最后。
  const ips = rows.filter(row => row.source.kind === 'ip');
  const rest = rows.filter(row => row.source.kind !== 'ip');
  const shown = open || ips.length <= FOLD + 1 ? ips : ips.slice(0, FOLD);
  const hidden = ips.slice(shown.length);
  const max = Math.max(0, ...rows.map(row => row.value));
  const scope = focus ? valueOf(chainOf.get(focus)!, metric) : whole;
  const row = ({ source, parts, value }: (typeof rows)[number]) => (
    <SourceRow
      key={source.key}
      source={source}
      parts={parts}
      value={value}
      max={max}
      whole={scope}
      chainOf={chainOf}
      metric={metric}
    />
  );
  return (
    <>
      {/* 只有一条链时组成条与图例没有新信息，读数栏已有它的合计。 */}
      {chains.length > 1 && (
        <>
          <div className="nid-compose" aria-hidden="true">
            {chains
              .filter(chain => valueOf(chain, metric) > 0)
              .map(chain => (
                <i
                  key={chain.key}
                  className={focus === chain.key ? 'on' : ''}
                  style={{ flexGrow: valueOf(chain, metric), background: chain.color }}
                />
              ))}
          </div>
          <div className="nti-legend nid-legend" role="group" aria-label="接入链（点击只看这条链的来源）">
            {chains.map(chain => (
              <button
                key={chain.key}
                type="button"
                className={`nti-key${focus === chain.key ? ' on' : ''}`}
                aria-pressed={focus === chain.key}
                onClick={() => {
                  setPicked(focus === chain.key ? null : chain.key);
                  setOpen(false);
                }}
              >
                <i className="nt-swatch" style={{ background: chain.color }} aria-hidden="true" />
                <ChainName chain={chain} />
                <b>{bytes(valueOf(chain, metric))}</b>
                <span className="nti-share">{shareOf(valueOf(chain, metric), whole)}</span>
              </button>
            ))}
          </div>
        </>
      )}
      <ol className="nid-lines" aria-label="来源">
        {shown.map(row)}
        {hidden.length > 0 && (
          <li className="nid-more-row">
            <button type="button" className="nid-more" aria-expanded={open} onClick={() => setOpen(true)}>
              其余 {hidden.length} 个来源 · {bytes(hidden.reduce((sum, item) => sum + item.value, 0))}
              <Icon of="unfold" size={12} />
            </button>
          </li>
        )}
        {open && ips.length > FOLD + 1 && (
          <li className="nid-more-row">
            <button type="button" className="nid-more" aria-expanded={open} onClick={() => setOpen(false)}>
              收起
              <Icon of="unfold" size={12} />
            </button>
          </li>
        )}
        {rest.map(row)}
      </ol>
    </>
  );
}

/* ── 弹窗 ── */

export function NodeInflowDetailsDialog({
  nodeId,
  nodeName,
  tenantId,
  userId,
  period,
  range,
  sourceOf = fallbackSource,
  colorOf = () => undefined,
  onClose,
}: {
  nodeId: string;
  nodeName?: string;
  tenantId: string;
  userId: string;
  period: SiteUsagePeriod;
  range?: TrafficTimeRange;
  sourceOf?: (key: string) => TrafficSource;
  /** 接入链在流入统计里的颜色；端口转发与未拆分的部分用弹窗自己的灰。 */
  colorOf?: (key: string) => string | undefined;
  onClose: () => void;
}) {
  const titleId = useId();
  const [metric, setMetric] = useState<Metric>('total');
  const query = useMetricQuery({
    queryKey: ['node-inflow-details', nodeId, tenantId, userId, period, range],
    queryFn: () => fetchNodeInflowDetails(nodeId, tenantId, userId, period, range),
    staleTime: 15_000,
    refetchInterval: 30_000,
    metricFacts: data => [data.limited, data.accounting_mismatch],
  });
  const model = useMemo(
    () => (query.data ? modelOf(query.data, sourceOf, colorOf) : null),
    [query.data, sourceOf, colorOf],
  );
  const label = query.data
    ? rangeLabel(period, range, query.data.range_start_unix_secs, query.data.range_end_unix_secs)
    : '';
  return (
    <DialogLayer label={`${userId} · 流入明细`} onClose={onClose}>
      <section className="dialog-surface node-inflow-dialog" aria-labelledby={titleId}>
        <header className="node-usage-header">
          <div className="nid-who">
            <GeneratedUserAvatar id={userId} variant="mini" />
            <h2 id={titleId}>{userId}</h2>
            <span className="nid-where">{nodeName || nodeId} · 流入明细</span>
          </div>
          <DialogClose className="btn sm node-usage-close" aria-label="关闭流入明细">
            <Icon of="close" size={16} />
          </DialogClose>
        </header>
        <div className="nid-body" aria-busy={query.isFetching}>
          {query.isPending && <PanelLoading label="加载来源流量明细" />}
          {query.isError && (
            <p role="alert" className="nid-state">
              {query.data ? '明细刷新失败，当前显示上次读取的数据。' : '明细读取失败。'}
              <button type="button" className="btn text sm" onClick={() => query.refetch()}>
                重试
              </button>
            </p>
          )}
          {/* 没有记录时只写一行，不排一列 0。 */}
          {model && model.sources.length === 0 && <p className="nid-state">{label}，没有已记录的流入。</p>}
          {query.data && model && model.sources.length > 0 && (
            <>
              <Readout model={model} label={label} />
              <Notices data={query.data} gap={model.gap} />
              <div className="nid-list">
                <div className="nid-listbar">
                  <span className="nid-listbar-title">来源</span>
                  <SegmentedControl
                    value={metric}
                    options={METRICS}
                    ariaLabel="按合计、下行或上行排序与比较"
                    onChange={setMetric}
                  />
                </div>
                <SourceRanking model={model} metric={metric} />
              </div>
            </>
          )}
        </div>
      </section>
    </DialogLayer>
  );
}
