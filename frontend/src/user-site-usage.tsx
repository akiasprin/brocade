import { useMetricQuery } from './ui/metric-cadence';
import { lazy, Suspense, useId, useMemo, useState } from 'react';
import { getDomain } from 'tldts';
import {
  fetchNodeOtherDomains,
  fetchNodeSiteUsage,
  fetchNodeSniffingFailedTargets,
  type NodeSiteUsage,
  fetchUserOtherDomains,
  fetchUserSiteUsage,
  fetchUserSniffingFailedTargets,
  type SiteUsagePeriod,
  type UserOtherDomains,
  type UserSiteUsage,
  type UserSniffingFailedTargets,
  type TrafficTimeRange,
} from './api';
import type { SiteUsageChartSeries, SiteUsageColor } from './site-usage-chart';
import { SegmentedControl } from './ui/bits';
import { bytes, shareOf } from './ui/format';
import { Icon, PanelTitle } from './ui/icons';
import { SiteIcon } from './ui/site-icon';
import { GeneratedUserAvatar } from './ui/user-avatar';
import { prefersReducedMotion } from './ui/motion';

/* 用户详情「站点流量」：固定站点分类、其他站点的域名明细、嗅探失败的目标明细合在一张卡里。
   上半部左侧是按小时（当天、当周）或按天（当月）的堆叠柱，右侧是排行；下半部是两类明细。
   范围是 UTC+8 的自然日、周一起的自然周、自然月，与额度的自然月相同；明细遵循数据保留设置，
   当周、当月的明细由说明行写明范围。 */

const LazySiteUsageChart = lazy(() => import('./site-usage-chart'));

const PERIODS = [
  { value: 'day', label: '当天' },
  { value: 'week', label: '当周' },
  { value: 'month', label: '当月' },
] as const satisfies readonly { value: SiteUsagePeriod; label: string }[];

type NonSite = 'other' | 'sniffing-failed' | 'unidentified';
const NON_SITE: readonly NonSite[] = ['other', 'sniffing-failed', 'unidentified'];
const isNonSite = (id: string): id is NonSite => (NON_SITE as readonly string[]).includes(id);
type DetailKind = 'other' | 'sniffing-failed';

/** 嗅探失败占已观测流量达到此比例时着金色：此时站点归类的覆盖面明显不足。 */
const FAILED_WARN_SHARE = 0.2;
/** 图上单独着色的站点数；其余站点合为一层。 */
const TOP_SITES = 5;
/** 每类明细默认列出的行数（两列各三行）。 */
const DETAIL_ROWS = 6;

const SCOPE =
  '认证代理入口按固定站点域名集合归类，每条连接只计入一类，不保存 URL。嗅探失败：启用嗅探但未取得域名；未识别：未启用嗅探且无域名。其他站点按主机名、嗅探失败按原始目标 IP 与端口另记明细，保存时长遵循数据保留设置。端口转发作为独立类型计入，沿用账本窗口起点归属；当天、当周按保留明细显示小时分布，当月按日汇总，不提供站点分类。站点采集状态只描述认证代理入口。按 UTC+8 自然日、自然周（周一起）、自然月统计。';
const NODE_SCOPE =
  '本机认证代理入口按目的站点分类，端口转发作为独立类型计入。端口转发沿用账本窗口起点归属，当天、当周按保留明细显示小时分布，当月按日汇总；不提供站点分类。站点采集状态只描述认证代理入口。按 UTC+8 自然日、自然周（周一起）、自然月统计。';
const FORWARDING_COLOR: SiteUsageColor = { token: '--observe-2' };
const NON_SITE_NOTE: Record<NonSite, string> = {
  other: '取得了域名，但不在预设站点集合内',
  'sniffing-failed': '启用了嗅探，但未取得可用域名',
  unidentified: '未启用嗅探，连接也未携带域名',
};

/* 分类色：固定本月主要五类站点的身份，跳过红、金两档（监控台上读作告警）；其余站点用中性档；
   未归类三类用墨色的三档透明度，嗅探失败超过阈值时改用金色。图与列表共用这一份。 */
const RANK_COLORS: readonly SiteUsageColor[] = [
  '--observe-1',
  '--observe-6',
  '--observe-4',
  '--observe-5',
  '--observe-7',
].map(token => ({ token }));
const REST_COLOR: SiteUsageColor = { token: '--observe-10' };
const NON_SITE_COLORS: Record<NonSite, SiteUsageColor> = {
  other: { token: '--ink', alpha: 0.44 },
  'sniffing-failed': { token: '--ink', alpha: 0.3 },
  unidentified: { token: '--ink', alpha: 0.19 },
};
const WARN_COLOR: SiteUsageColor = { token: '--gold' };
const cssColor = ({ token, alpha }: SiteUsageColor) =>
  alpha === undefined ? `var(${token})` : `color-mix(in srgb, var(${token}) ${Math.round(alpha * 100)}%, transparent)`;

/* 时刻按 UTC+8 显示，与周期边界一致。 */
const ZONE_SECS = 8 * 3600;
const WEEK = ['日', '一', '二', '三', '四', '五', '六'];
const two = (value: number) => String(value).padStart(2, '0');
const wall = (unixSecs: number) => new Date((unixSecs + ZONE_SECS) * 1000);
const hm = (unixSecs: number) => `${two(wall(unixSecs).getUTCHours())}:${two(wall(unixSecs).getUTCMinutes())}`;
const md = (unixSecs: number) => `${two(wall(unixSecs).getUTCMonth() + 1)}-${two(wall(unixSecs).getUTCDate())}`;
const weekday = (unixSecs: number) => `周${WEEK[wall(unixSecs).getUTCDay()]}`;
const dayOf = (unixSecs: number) => Math.floor((unixSecs + ZONE_SECS) / 86_400);
const moment = (unixSecs: number, nowUnixSecs: number) => {
  const days = dayOf(nowUnixSecs) - dayOf(unixSecs);
  return days === 0 ? `今天 ${hm(unixSecs)}` : days === 1 ? `昨天 ${hm(unixSecs)}` : `${md(unixSecs)} ${hm(unixSecs)}`;
};
/** 当天写日期与星期；当周、当月写整个周期的起止日，读者据此知道一周从周一算起。 */
const periodDates = (data: UserSiteUsage) =>
  data.period === 'day'
    ? `${md(data.range_start_unix_secs)} ${weekday(data.range_start_unix_secs)}`
    : `${md(data.range_start_unix_secs)} – ${md(data.period_end_unix_secs - 86_400)}`;

const amount = (value: number) => {
  const [figure, unit] = bytes(value).split(' ');
  return (
    <>
      {figure}
      <small>{unit}</small>
    </>
  );
};

interface Category {
  id: string;
  name: string;
  up: number;
  down: number;
  total: number;
}

function categoriesOf(data: UserSiteUsage) {
  const rows: Category[] = data.rows
    .map(row => ({
      id: row.site_id,
      name: row.name,
      up: row.uplink_bytes,
      down: row.downlink_bytes,
      total: row.uplink_bytes + row.downlink_bytes,
    }))
    .filter(row => row.total > 0)
    .sort((a, b) => b.total - a.total || a.id.localeCompare(b.id));
  const total = rows.reduce((sum, row) => sum + row.total, 0);
  const sites = rows.filter(row => !isNonSite(row.id) && row.id !== 'forwarding');
  const forwarding = rows.find(row => row.id === 'forwarding');
  const unsorted = NON_SITE.map(id => rows.find(row => row.id === id)).filter((row): row is Category => !!row);
  const failed = unsorted.find(row => row.id === 'sniffing-failed');
  const observedTotal = total - (forwarding?.total ?? 0);
  return {
    rows,
    total,
    sites,
    forwarding,
    unsorted,
    up: rows.reduce((sum, row) => sum + row.up, 0),
    down: rows.reduce((sum, row) => sum + row.down, 0),
    failedWarn: observedTotal > 0 && (failed?.total ?? 0) / observedTotal >= FAILED_WARN_SHARE,
  };
}
type Categories = ReturnType<typeof categoriesOf>;

function colorOf(categories: Categories, id: string, palette: readonly string[] = []): SiteUsageColor {
  if (id === 'forwarding') return FORWARDING_COLOR;
  if (isNonSite(id)) return id === 'sniffing-failed' && categories.failedWarn ? WARN_COLOR : NON_SITE_COLORS[id];
  return RANK_COLORS[palette.indexOf(id)] ?? REST_COLOR;
}

function siteGroups(categories: Categories, palette: readonly string[]) {
  const featured = new Set(palette.slice(0, TOP_SITES));
  return {
    top: categories.sites.filter(site => featured.has(site.id)),
    rest: categories.sites.filter(site => !featured.has(site.id)),
  };
}

/** 固定本月主要五类站点着色，其余合为一层；周期内尚未到的格为 null。 */
function chartSeries(data: UserSiteUsage, categories: Categories, palette: readonly string[]): SiteUsageChartSeries[] {
  const count = Math.round((data.period_end_unix_secs - data.range_start_unix_secs) / data.bucket_secs);
  const perSite = new Map(data.series.map(series => [series.site_id, series.points]));
  const values = (ids: string[]) => {
    const out: (number | null)[] = Array.from({ length: count }, (_, index) =>
      ids[0] !== 'forwarding' && data.range_start_unix_secs + index * data.bucket_secs < data.range_end_unix_secs
        ? 0
        : null,
    );
    for (const id of ids) {
      for (const [at, up, down] of perSite.get(id) ?? []) {
        const index = Math.floor((at - data.range_start_unix_secs) / data.bucket_secs);
        if (index >= 0 && index < count) out[index] = (out[index] ?? 0) + up + down;
      }
    }
    return out;
  };
  const { top, rest } = siteGroups(categories, palette);
  const series: SiteUsageChartSeries[] = top.map(site => ({
    key: site.id,
    name: site.name,
    color: colorOf(categories, site.id, palette),
    values: values([site.id]),
  }));
  if (rest.length > 0) {
    series.push({
      key: 'rest',
      name: `其余 ${rest.length} 类站点`,
      color: REST_COLOR,
      values: values(rest.map(site => site.id)),
    });
  }
  if (categories.forwarding) {
    series.unshift({
      key: 'forwarding',
      name: categories.forwarding.name,
      color: FORWARDING_COLOR,
      values: values(['forwarding']),
    });
  }
  for (const row of categories.unsorted) {
    series.push({ key: row.id, name: row.name, color: colorOf(categories, row.id), values: values([row.id]) });
  }
  return series;
}

/* ── 明细 ── */

// Include private suffixes so unrelated tenants on github.io / blogspot.com stay separate.
// The bundled public suffix table requires no network access; bare suffixes remain intact.
export function mainDomain(host: string): string {
  return getDomain(host, { allowPrivateDomains: true, extractHostname: false }) ?? host;
}

interface DomainGroup {
  key: string;
  main: string;
  hosts: { host: string; total: number }[];
  up: number;
  down: number;
  total: number;
}

export function domainGroups(rows: UserOtherDomains['rows']): DomainGroup[] {
  const groups = new Map<string, DomainGroup>();
  for (const row of rows) {
    const main = mainDomain(row.domain);
    const group = groups.get(main) ?? { key: main, main, hosts: [], up: 0, down: 0, total: 0 };
    const total = row.uplink_bytes + row.downlink_bytes;
    group.hosts.push({ host: row.domain, total });
    group.up += row.uplink_bytes;
    group.down += row.downlink_bytes;
    group.total += total;
    groups.set(main, group);
  }
  return [...groups.values()].sort((a, b) => b.total - a.total || a.main.localeCompare(b.main));
}

/** 端口的约定用途；只标注端口号本身的含义，不推测目标是哪个站点。 */
const PORT_NAMES: Record<string, string> = {
  '53': 'DNS',
  '80': 'HTTP',
  '853': 'DoT',
  '3478': 'STUN',
  '5222': 'XMPP',
  '8080': 'HTTP',
  '8443': 'HTTPS',
};

interface TargetItem {
  key: string;
  protocol: string;
  host: string;
  port: string;
  portName: string | null;
  up: number;
  down: number;
  total: number;
}

export function targetItems(rows: UserSniffingFailedTargets['rows']): TargetItem[] {
  return rows
    .map(row => {
      const match = /^(tcp|udp):\/\/(\[[0-9a-f:]+\]|[0-9.]+):(\d+)$/i.exec(row.target);
      const protocol = match?.[1].toUpperCase() ?? '';
      const port = match?.[3] ?? '';
      return {
        key: row.target,
        protocol,
        host: match?.[2] ?? row.target,
        port,
        portName: port === '443' ? (protocol === 'UDP' ? 'QUIC' : 'HTTPS') : (PORT_NAMES[port] ?? null),
        up: row.uplink_bytes,
        down: row.downlink_bytes,
        total: row.uplink_bytes + row.downlink_bytes,
      };
    })
    .sort((a, b) => b.total - a.total || a.key.localeCompare(b.key));
}

const DETAIL: Record<DetailKind, { label: string; by: string; unit: string; capped: string }> = {
  other: { label: '其他站点', by: '按主域名', unit: '个主域名', capped: '个主机' },
  'sniffing-failed': { label: '嗅探失败', by: '按目标 IP 与端口', unit: '个目标', capped: '个目标' },
};

/** 明细的采集范围：只写当前成立的几项。 */
function coverageText(
  kind: DetailKind,
  detail: UserOtherDomains | UserSniffingFailedTargets,
  site: UserSiteUsage | undefined,
  categoryTotal: number,
): string {
  const parts: string[] = [];
  if (!detail.collection_enabled) parts.push('明细采集已关闭，显示已保留的数据');
  if (!site || detail.range_start_unix_secs > site.range_start_unix_secs)
    parts.push(`明细自${moment(detail.range_start_unix_secs, detail.range_end_unix_secs)} 起保留`);
  if (detail.observed_since_unix_secs !== null && detail.observed_since_unix_secs > detail.range_start_unix_secs)
    parts.push(`自${moment(detail.observed_since_unix_secs, detail.range_end_unix_secs)} 起采集`);
  const covered = detail.rows.reduce((sum, row) => sum + row.uplink_bytes + row.downlink_bytes, 0);
  parts.push(
    `覆盖 ${bytes(covered)}${categoryTotal > 0 ? `，占${DETAIL[kind].label} ${shareOf(covered, categoryTotal)}` : ''}`,
  );
  if (detail.rows.length >= detail.top_n) parts.push(`只保留前 ${detail.top_n} ${DETAIL[kind].capped}`);
  if (detail.reporting_nodes < detail.expected_nodes)
    parts.push(`${detail.reporting_nodes} / ${detail.expected_nodes} 台入口已上报`);
  if (detail.limited) parts.push('计数受限，部分流量未计入');
  return parts.join(' · ');
}

function DomainRow({ group, max, color }: { group: DomainGroup; max: number; color: string }) {
  const single = group.hosts.length === 1 ? group.hosts[0].host : null;
  const sub = single && single !== group.main ? single.slice(0, -group.main.length) : '';
  return (
    <li className="site-usage-drow" title={group.hosts.map(host => `${host.host} · ${bytes(host.total)}`).join('\n')}>
      <span className="site-usage-dname">
        <span className="site-usage-host">
          {sub && <span className="site-usage-dsub">{sub}</span>}
          <span className="site-usage-dmain">{group.main}</span>
        </span>
        {group.hosts.length > 1 && <span className="site-usage-dnote">{group.hosts.length} 个主机</span>}
      </span>
      <span className="site-usage-dbar" aria-hidden="true">
        <i style={{ width: `${(group.total / max) * 100}%`, background: color }} />
      </span>
      <span className="site-usage-num">{amount(group.total)}</span>
    </li>
  );
}

function TargetRow({ item, max, color }: { item: TargetItem; max: number; color: string }) {
  return (
    <li
      className="site-usage-drow site-usage-target-row"
      title={`${item.key} · 下行 ${bytes(item.down)} · 上行 ${bytes(item.up)}`}
    >
      <span className="site-usage-dname site-usage-target">
        {item.protocol && <span className="site-usage-proto">{item.protocol}</span>}
        <span className="site-usage-dmain">
          {item.host}
          {item.port && <span className="site-usage-port">:{item.port}</span>}
        </span>
        {item.portName && <span className="site-usage-dnote">{item.portName}</span>}
      </span>
      <span className="site-usage-dbar" aria-hidden="true">
        <i style={{ width: `${(item.total / max) * 100}%`, background: color }} />
      </span>
      <span className="site-usage-num">{amount(item.total)}</span>
    </li>
  );
}

function DetailSection({
  id,
  kind,
  query,
  site,
  categoryTotal,
  color,
  open,
  onToggle,
}: {
  id: string;
  kind: DetailKind;
  query: { data?: UserOtherDomains | UserSniffingFailedTargets; isPending: boolean; isError: boolean };
  site: UserSiteUsage | undefined;
  categoryTotal: number;
  color: SiteUsageColor;
  open: boolean;
  onToggle: () => void;
}) {
  const [all, setAll] = useState(false);
  const config = DETAIL[kind];
  const detail = query.data;
  const items = useMemo(
    () =>
      !detail
        ? []
        : kind === 'other'
          ? domainGroups((detail as UserOtherDomains).rows)
          : targetItems((detail as UserSniffingFailedTargets).rows),
    [detail, kind],
  );
  const label = (item: DomainGroup | TargetItem) =>
    'main' in item ? item.main : `${item.protocol} ${item.host}:${item.port}`;
  const preview = query.isError
    ? '明细读取失败'
    : !detail
      ? '正在读取明细…'
      : items.length === 0
        ? '暂无明细'
        : `${items.slice(0, 3).map(label).join('、')}${items.length > 3 ? ` 等 ${items.length} ${config.unit}` : ''}`;
  const shown = all ? items : items.slice(0, DETAIL_ROWS);
  const half = Math.ceil(shown.length / 2);
  const columns = shown.length > 2 ? [shown.slice(0, half), shown.slice(half)] : [shown];
  const max = items[0]?.total || 1;
  const swatch = cssColor(color);
  return (
    <section className={`site-usage-detail${open ? ' open' : ''}`} id={id} aria-label={`${config.label}明细`}>
      <button type="button" className="site-usage-detail-head" aria-expanded={open} onClick={onToggle}>
        <i className="site-usage-swatch" style={{ background: swatch }} aria-hidden="true" />
        <span className="site-usage-detail-title">{config.label}</span>
        <span className="site-usage-detail-by">{config.by}</span>
        {open && detail ? (
          <span className="site-usage-detail-count">
            {items.length} {config.unit}
          </span>
        ) : (
          <span className="site-usage-detail-preview">{preview}</span>
        )}
        <Icon of="chevronDown" size={11} className="site-usage-chevron" />
      </button>
      {open && (
        <>
          <p
            className={`site-usage-detail-note${query.isError ? ' bad' : ''}`}
            role={query.isError ? 'alert' : undefined}
          >
            {query.isError
              ? `${config.label}明细读取失败，请稍后重试。`
              : !detail
                ? '正在读取明细…'
                : `${coverageText(kind, detail, site, categoryTotal)}${items.length === 0 ? ' · 暂无记录' : ''}`}
          </p>
          {items.length > 0 && (
            <div className={`site-usage-dcols${columns.length === 1 ? ' one' : ''}`}>
              {columns.map((column, index) => (
                <ol key={index} className="site-usage-drows">
                  {column.map(item =>
                    'main' in item ? (
                      <DomainRow key={item.key} group={item} max={max} color={swatch} />
                    ) : (
                      <TargetRow key={item.key} item={item} max={max} color={swatch} />
                    ),
                  )}
                </ol>
              ))}
            </div>
          )}
          {items.length > DETAIL_ROWS && (
            <button type="button" className="site-usage-more" onClick={() => setAll(value => !value)}>
              {all ? '收起' : `显示全部 ${items.length} ${config.unit}`}
            </button>
          )}
        </>
      )}
    </section>
  );
}

/* ── 卡片 ── */

function ReportStatus({ data, machine = false }: { data: UserSiteUsage; machine?: boolean }) {
  const withForwarding = machine || data.rows.some(row => row.site_id === 'forwarding');
  if (!data.collection_enabled) {
    return (
      <span className="rut-status" role="status">
        <i className="node-lamp idle" />
        {withForwarding ? '站点采集已关闭 · 转发仍按账本统计' : '采集已关闭 · 显示已保留的数据'}
      </span>
    );
  }
  if (withForwarding && data.expected_nodes === 0) {
    return (
      <span className="rut-status" role="status">
        <i className="node-lamp idle" />
        {machine ? '本机无认证代理入口' : '无认证代理入口'} · 转发按账本统计
      </span>
    );
  }
  const warn = data.reporting_nodes < data.expected_nodes || data.limited;
  return (
    <span
      className={`rut-status${warn ? ' warn' : ''}`}
      role="status"
      title={
        withForwarding
          ? '近 5 分钟内上报站点统计的认证入口机器，不表示转发采集状态'
          : '近 5 分钟内上报站点统计的入口机器 / 该用户已授权的入口机器'
      }
    >
      <i className={`node-lamp${warn ? ' warn' : ''}`} />
      <b>{data.reporting_nodes}</b> / {data.expected_nodes} 台入口已上报{data.limited ? ' · 计数受限' : ''}
    </span>
  );
}

function unsupportedText(data: UserSiteUsage, machine: boolean): string {
  if (data.expected_nodes === 0)
    return machine ? '本机尚无已记录的流出流量。端口转发上报后将单独列出。' : '尚无已记录的站点或端口转发流量。';
  if (data.latest_report_unix_secs !== null) return '入口机器的站点上报已过期，所选范围内没有可用数据。';
  return '尚无站点观测数据。需入口机器的 Agent 与 Xray 都支持站点统计并完成上报。';
}

export function UserSiteUsageCard({ tenantId, userId }: { tenantId: string; userId: string }) {
  return <SiteUsageCard scope={{ tenantId, userId }} />;
}

export function NodeSiteUsageCard({
  nodeId,
  period,
  range,
}: {
  nodeId: string;
  period: SiteUsagePeriod;
  range?: TrafficTimeRange;
}) {
  return <SiteUsageCard key={nodeId} scope={{ nodeId }} controlledPeriod={period} range={range} />;
}

type SiteScope = { tenantId: string; userId: string } | { nodeId: string };
function SiteUsageCard({
  scope,
  controlledPeriod,
  range,
}: {
  scope: SiteScope;
  controlledPeriod?: SiteUsagePeriod;
  range?: TrafficTimeRange;
}) {
  const [localPeriod, setPeriod] = useState<SiteUsagePeriod>('day');
  const period = controlledPeriod ?? localPeriod;
  const nodeId = 'nodeId' in scope ? scope.nodeId : null;
  const tenantId = 'tenantId' in scope ? scope.tenantId : '';
  const userId = 'userId' in scope ? scope.userId : '';
  const title = nodeId === null ? '站点流量' : '流出统计';
  const [selectedSite, setSelectedSite] = useState<string | null>(null);
  const [restOpen, setRestOpen] = useState(false);
  const [open, setOpen] = useState<Record<DetailKind, boolean>>({ other: true, 'sniffing-failed': true });
  const [focus, setFocus] = useState<string | null>(null);
  const uid = useId();
  // 切换周期时保留上一周期的读数，换了用户则不沿用。
  const rangeArgs = range ? ([range] as const) : ([] as const);
  const sameUser = (key: readonly unknown[] | undefined) =>
    key?.[1] === (nodeId ?? tenantId) && key[2] === userId && JSON.stringify(key[4]) === JSON.stringify(range);
  const scopeKey = nodeId === null ? 'user' : 'node';
  const site = useMetricQuery({
    queryKey: [`${scopeKey}-site-usage`, nodeId ?? tenantId, userId, period, ...rangeArgs],
    queryFn: () =>
      nodeId === null ? fetchUserSiteUsage(tenantId, userId, period) : fetchNodeSiteUsage(nodeId, period, ...rangeArgs),
    refetchInterval: 30_000,
    staleTime: 15_000,
    placeholderData: (previous, previousQuery) => (sameUser(previousQuery?.queryKey) ? previous : undefined),
  });
  // One monthly reference assigns identities in every range. Day/week ranking never
  // reassigns colors; the month query shares the normal query cache with the month tab.
  const month = useMetricQuery({
    queryKey: [`${scopeKey}-site-usage`, nodeId ?? tenantId, userId, 'month'],
    queryFn: () =>
      nodeId === null ? fetchUserSiteUsage(tenantId, userId, 'month') : fetchNodeSiteUsage(nodeId, 'month'),
    staleTime: 15_000,
    refetchInterval: 30_000,
  });
  const domains = useMetricQuery({
    queryKey: [`${scopeKey}-other-domains`, nodeId ?? tenantId, userId, period, ...rangeArgs],
    queryFn: () =>
      nodeId === null
        ? fetchUserOtherDomains(tenantId, userId, period)
        : fetchNodeOtherDomains(nodeId, period, ...rangeArgs),
    refetchInterval: 30_000,
    staleTime: 15_000,
    placeholderData: (previous, previousQuery) => (sameUser(previousQuery?.queryKey) ? previous : undefined),
  });
  const targets = useMetricQuery({
    queryKey: [`${scopeKey}-sniffing-failed-targets`, nodeId ?? tenantId, userId, period, ...rangeArgs],
    queryFn: () =>
      nodeId === null
        ? fetchUserSniffingFailedTargets(tenantId, userId, period)
        : fetchNodeSniffingFailedTargets(nodeId, period, ...rangeArgs),
    refetchInterval: 30_000,
    staleTime: 15_000,
    placeholderData: (previous, previousQuery) => (sameUser(previousQuery?.queryKey) ? previous : undefined),
  });
  const data: (UserSiteUsage & { top_users?: NodeSiteUsage['top_users'] }) | undefined = site.data;
  const categories = useMemo(() => (data ? categoriesOf(data) : null), [data]);
  const monthOrder = useMemo(
    () => (month.data ? categoriesOf(month.data).sites.map(site => site.id) : []),
    [month.data],
  );
  const paletteScope = `${nodeId ?? tenantId}/${userId}/${month.data?.range_start_unix_secs ?? ''}`;
  const [assignment, setAssignment] = useState<{ scope: string; ids: string[] }>({ scope: '', ids: [] });
  // Color assignment is presentation state: append newly observed sites without recoloring
  // an existing identity when live monthly totals overtake each other.
  const newIds = assignment.scope === paletteScope ? monthOrder.filter(id => !assignment.ids.includes(id)) : monthOrder;
  const palette = assignment.scope === paletteScope ? assignment.ids : monthOrder;
  if (month.data && (assignment.scope !== paletteScope || newIds.length > 0)) {
    setAssignment({
      scope: paletteScope,
      ids: assignment.scope === paletteScope ? [...assignment.ids, ...newIds] : monthOrder,
    });
  }
  const series = useMemo(
    () => (data && categories ? chartSeries(data, categories, palette) : []),
    [data, categories, palette],
  );
  const groups = categories ? siteGroups(categories, palette) : { top: [], rest: [] };
  const forwardingPlaced =
    data?.series
      .find(series => series.site_id === 'forwarding')
      ?.points.reduce((sum, point) => sum + point[1] + point[2], 0) ?? 0;
  const forwardingUnplaced = Math.max(0, (categories?.forwarding?.total ?? 0) - forwardingPlaced);
  const supported = data && (data.reporting_nodes > 0 || data.rows.length > 0);
  const detailId = (kind: DetailKind) => `${uid}-${kind}`;
  const showDetail = (kind: DetailKind) => {
    setOpen(current => ({ ...current, [kind]: true }));
    requestAnimationFrame(() =>
      document.getElementById(detailId(kind))?.scrollIntoView?.({
        block: 'nearest',
        behavior: prefersReducedMotion() ? 'instant' : 'smooth',
      }),
    );
  };

  const head = data && categories && (
    <div className="site-usage-head">
      {categories.total > 0 ? (
        <>
          <span className="site-usage-total">{amount(categories.total)}</span>
          <span className="site-usage-label">
            {periodDates(data)} · {categories.sites.length} 类站点{categories.forwarding ? ' · 端口转发' : ''}
          </span>
          <span className="site-usage-meta">
            {data.observed_since_unix_secs !== null && data.observed_since_unix_secs > data.range_start_unix_secs && (
              <>
                <span className="warn" title="此前至少一台入口机器尚未开始站点统计，所选范围内更早的站点流量未计入">
                  {nodeId !== null || categories.forwarding ? '站点观测起点' : '观测起点'}{' '}
                  {moment(data.observed_since_unix_secs, data.range_end_unix_secs)}
                </span>
                {' · '}
              </>
            )}
            下行 <b>{bytes(categories.down)}</b> · 上行 <b>{bytes(categories.up)}</b>
          </span>
        </>
      ) : (
        <span className="site-usage-label">
          {periodDates(data)} · {PERIODS.find(option => option.value === data.period)?.label}没有观测到流量
        </span>
      )}
      {controlledPeriod === undefined && (
        <SegmentedControl ariaLabel="站点统计范围" value={period} options={PERIODS} onChange={setPeriod} />
      )}
    </div>
  );

  const item = (row: Category, sub = false) => {
    const topUsers = data?.top_users?.find(entry => entry.site_id === row.id);
    const selected = selectedSite === row.id;
    const jump = (row.id === 'other' || row.id === 'sniffing-failed') && categories ? (row.id as DetailKind) : null;
    const swatch = sub ? 'transparent' : cssColor(colorOf(categories!, row.id, palette));
    const content = (
      <>
        <i className="site-usage-swatch" style={{ background: swatch }} aria-hidden="true" />
        {row.id === 'forwarding' ? (
          <span className="site-brand-icon" aria-hidden="true">
            <Icon of="forwarding" size={18} />
          </span>
        ) : (
          <SiteIcon siteId={row.id} />
        )}
        <span className="site-usage-name">{row.name}</span>
        <span className="site-usage-num">{amount(row.total)}</span>
        <span className={`site-usage-pct${row.id === 'sniffing-failed' && categories!.failedWarn ? ' warn' : ''}`}>
          {shareOf(row.total, categories!.total)}
        </span>
      </>
    );
    const focusKey = sub
      ? 'rest'
      : row.id === 'forwarding' || isNonSite(row.id) || palette.slice(0, TOP_SITES).includes(row.id)
        ? row.id
        : 'rest';
    const title = `${row.name} · 下行 ${bytes(row.down)} · 上行 ${bytes(row.up)}${
      isNonSite(row.id) ? `\n${NON_SITE_NOTE[row.id]}` : ''
    }`;
    return (
      <li
        key={row.id}
        className={`site-usage-item${sub ? ' sub' : ''}${isNonSite(row.id) ? ' muted' : ''}${selected ? ' open' : ''}`}
        onMouseEnter={() => setFocus(focusKey)}
        onMouseLeave={() => setFocus(null)}
      >
        {nodeId !== null ? (
          <button
            type="button"
            className="site-usage-row"
            title={title}
            aria-expanded={selected}
            onClick={() => setSelectedSite(selected ? null : row.id)}
            onFocus={() => setFocus(focusKey)}
            onBlur={() => setFocus(null)}
          >
            {content}
          </button>
        ) : jump ? (
          <button
            type="button"
            className="site-usage-row"
            title={`${title}\n查看明细`}
            onClick={() => showDetail(jump)}
          >
            {content}
          </button>
        ) : (
          <span className="site-usage-row" title={title}>
            {content}
          </span>
        )}
        {nodeId !== null && selected && (
          <div className="node-site-users" aria-label={`${row.name}的主要用户`}>
            <span className="site-usage-note">本机主要用户</span>
            <ul>
              {topUsers?.users.map(user => (
                <li
                  key={`${user.tenant_id}/${user.user_id}`}
                  title={`下行 ${bytes(user.downlink_bytes)} · 上行 ${bytes(user.uplink_bytes)}`}
                >
                  <GeneratedUserAvatar id={user.user_id} variant="mini" />
                  <span className="nti-name" title={user.user_id}>
                    {user.user_id}
                  </span>
                  <b>{bytes(user.uplink_bytes + user.downlink_bytes)}</b>
                  <span className="nti-share">{shareOf(user.uplink_bytes + user.downlink_bytes, row.total)}</span>
                </li>
              ))}
            </ul>
            {!!topUsers?.other_users && (
              <p className="site-usage-note">
                其余 {topUsers.other_users} 位用户 ·{' '}
                {bytes(topUsers.other_uplink_bytes + topUsers.other_downlink_bytes)}
              </p>
            )}
            {!topUsers && <p className="site-usage-note">没有用户明细</p>}
            {jump && (
              <button type="button" className="btn text sm" onClick={() => showDetail(jump)}>
                查看{row.name}明细
              </button>
            )}
          </div>
        )}
      </li>
    );
  };

  return (
    <section className="panel config-panel user-dcard site-usage" aria-label={title}>
      <header>
        <span title={nodeId === null ? SCOPE : NODE_SCOPE}>
          <PanelTitle of="usage">{title}</PanelTitle>
        </span>
        {data && <ReportStatus data={data} machine={nodeId !== null} />}
      </header>
      <div
        className="site-usage-body panel-full-bleed-body"
        aria-busy={site.isFetching || domains.isFetching || targets.isFetching}
      >
        {site.isError && !data && (
          <p role="alert" className="site-usage-state">
            站点统计读取失败，请稍后重试。
          </p>
        )}
        {site.isError && data && (
          <p role="alert" className="site-usage-state">
            站点统计刷新失败，当前显示上次读取的数据。
          </p>
        )}
        {site.isPending && (
          <p className="site-usage-state" role="status">
            正在读取站点统计…
          </p>
        )}
        {data && !supported && <p className="site-usage-state">{unsupportedText(data, nodeId !== null)}</p>}
        {data && !supported && <div className="site-usage-plot">{head}</div>}
        {!data && site.isError && (
          <div className="site-usage-head">
            {controlledPeriod === undefined && (
              <SegmentedControl ariaLabel="站点统计范围" value={period} options={PERIODS} onChange={setPeriod} />
            )}
          </div>
        )}
        {data && supported && categories && (
          <>
            <div className="site-usage-main">
              <section className="site-usage-plot" aria-label="流量随时间分布">
                {head}
                {categories.total > 0 && (
                  <>
                    <Suspense fallback={<div className="site-usage-chart" />}>
                      <LazySiteUsageChart
                        period={data.period}
                        startUnixSecs={data.range_start_unix_secs}
                        bucketSecs={data.bucket_secs}
                        nowUnixSecs={data.range_end_unix_secs}
                        observedSinceUnixSecs={
                          !categories.forwarding &&
                          data.observed_since_unix_secs !== null &&
                          data.observed_since_unix_secs > data.range_start_unix_secs
                            ? data.observed_since_unix_secs
                            : null
                        }
                        series={series}
                        focusKey={focus}
                      />
                    </Suspense>
                    <p className="site-usage-note">
                      {data.period === 'month' ? '每天' : '每小时'}一柱 · 固定本月主要 {TOP_SITES}{' '}
                      类站点着色，其余站点合为一层{categories.forwarding ? '；端口转发单独显示' : ''}
                    </p>
                    {forwardingUnplaced > 0 && (
                      <p className="site-usage-note">
                        端口转发有 {bytes(forwardingUnplaced)} 已计入总量，小时明细已不完整，未分配到图中。
                      </p>
                    )}
                  </>
                )}
              </section>
              <section className="site-usage-side" aria-label={nodeId === null ? '各站点流量' : '各类型流量'}>
                {categories.total === 0 ? (
                  <p className="site-usage-empty">
                    {PERIODS.find(option => option.value === data.period)?.label}没有站点记录
                  </p>
                ) : (
                  <ol className="site-usage-key">
                    {categories.forwarding && item(categories.forwarding)}
                    {groups.top.map(row => item(row))}
                    {groups.rest.length > 0 && (
                      <li
                        className="site-usage-item"
                        onMouseEnter={() => setFocus('rest')}
                        onMouseLeave={() => setFocus(null)}
                      >
                        <button
                          type="button"
                          className="site-usage-row"
                          aria-expanded={restOpen}
                          onClick={() => setRestOpen(value => !value)}
                          title={groups.rest.map(row => row.name).join('、')}
                        >
                          <i
                            className="site-usage-swatch"
                            style={{ background: cssColor(REST_COLOR) }}
                            aria-hidden="true"
                          />
                          <Icon of="chevronDown" size={11} className={`site-usage-chevron${restOpen ? ' open' : ''}`} />
                          <span className="site-usage-name">其余 {groups.rest.length} 类站点</span>
                          <span className="site-usage-num">
                            {amount(groups.rest.reduce((sum, row) => sum + row.total, 0))}
                          </span>
                          <span className="site-usage-pct">
                            {shareOf(
                              groups.rest.reduce((sum, row) => sum + row.total, 0),
                              categories.total,
                            )}
                          </span>
                        </button>
                      </li>
                    )}
                    {restOpen && groups.rest.map(row => item(row, true))}
                    {categories.unsorted.length > 0 && (
                      <li className="site-usage-group" aria-hidden="true">
                        未归类
                      </li>
                    )}
                    {categories.unsorted.map(row => item(row))}
                  </ol>
                )}
              </section>
            </div>
          </>
        )}
        {(['other', 'sniffing-failed'] as const)
          .filter(kind => {
            const detail = kind === 'other' ? domains : targets;
            return (
              detail.isError || categories?.rows.some(row => row.id === kind) || (detail.data?.rows.length ?? 0) > 0
            );
          })
          .map(kind => (
            <DetailSection
              key={kind}
              id={detailId(kind)}
              kind={kind}
              query={kind === 'other' ? domains : targets}
              site={data}
              categoryTotal={
                site.isPlaceholderData || (kind === 'other' ? domains : targets).isPlaceholderData
                  ? 0
                  : (categories?.rows.find(row => row.id === kind)?.total ?? 0)
              }
              color={categories ? colorOf(categories, kind) : NON_SITE_COLORS[kind]}
              open={open[kind]}
              onToggle={() => setOpen(current => ({ ...current, [kind]: !current[kind] }))}
            />
          ))}
        {range && data && (
          <p className="site-usage-note">
            目标按小时汇总，实际覆盖 {moment(data.range_start_unix_secs, data.range_end_unix_secs)} —{' '}
            {moment(data.range_end_unix_secs, data.range_end_unix_secs)}
            （UTC+8）；不按分钟均摊站点流量，转发仅含尚存的窗口明细。
          </p>
        )}
      </div>
    </section>
  );
}
