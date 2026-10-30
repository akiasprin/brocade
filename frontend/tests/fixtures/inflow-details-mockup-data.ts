// Synthetic Console for the 流入明细 mockup (inflow-details-mockup.html). Never imported by the app.
// The machine usage dialog behind the details runs on the typical scene of node-traffic-mockup-data.ts. The details
// split each user × chain cell of that scene into source IPs, so the dialog's totals match the 流入统计 card.
// Addresses come only from the documentation ranges (RFC 5737, RFC 3849).
import type {
  ConsoleSnapshot,
  NetworkOperator,
  NodeInflowDetailRow,
  NodeInflowDetails,
  NodeInflowUsage,
  NodeSiteUsage,
  NodeTrafficHistory,
  SiteUsagePeriod,
} from '../../src/api';
import type { NodeRealtimeEvent } from '../../src/node-realtime';
import {
  aggregate,
  build,
  chainViews,
  hash,
  inflowOf,
  liveHistory,
  liveRates,
  mulberry,
  NOW,
  SCENES,
  siteRowsOf,
  type ChainView,
} from './node-traffic-mockup-data';

export const NODE_ID = 'n1';
export const NODE_NAME = '香港测试机器';

const world = SCENES.typical();
const built = build(world);
const hour = liveHistory(built);

export const DETAIL_SCENES = [
  { value: 'typical', label: '典型：多条链、IPv4 与 IPv6' },
  { value: 'mobile', label: '手机网络：大量来源' },
  { value: 'single', label: '单链单来源' },
  { value: 'degraded', label: '来源不完整、与账本不一致' },
  { value: 'empty', label: '所选时段没有记录' },
  { value: 'error', label: '读取失败' },
] as const;
export type DetailScene = (typeof DETAIL_SCENES)[number]['value'];
let detailScene: DetailScene = 'typical';
export function selectDetailScene(next: DetailScene) {
  detailScene = next;
}

/** Users that have inflow in the typical scene, busiest first, for the review bar. */
export const DETAIL_USERS = inflowOf(world, aggregate(built, 'month'), views('month')).rows.map(row => ({
  id: row.id,
  tenant: row.tenant,
}));

function views(period: SiteUsagePeriod): ChainView[] {
  return chainViews(world, aggregate(built, period), aggregate(built, 'month'));
}

/** Chain colors follow the 流入统计 card: the month's top five chains, then neutral grays. */
export function chainColor(id: string): string {
  return views('month').find(view => view.id === id)?.color ?? 'color-mix(in srgb, var(--observe-10) 75%, transparent)';
}

export function snapshotOf(): ConsoleSnapshot {
  return {
    redacted: false,
    node_egress_dns: [],
    snapshot: {
      revision: 1,
      apps: [
        {
          id: 'app',
          label: '示例',
          chains: world.chains.map(chain => ({
            id: chain.id,
            name: chain.name,
            tenant: 'platform',
            subscription_country: chain.country,
          })),
          ingresses: world.chains.map((chain, i) => ({
            id: chain.id,
            chain: chain.id,
            node: NODE_ID,
            port: 2000 + i,
            bind: '0.0.0.0',
            projection: {},
            guard: {
              no_private: false,
              no_bittorrent: false,
              no_mail: false,
              no_udp_amplification: false,
              tcp_and_quic_only: false,
              cn_source_block: 'off',
            },
            identity: {},
            wires: {},
          })),
          fronts: [],
          steps: [],
          grants: [...world.grants].flatMap(key => {
            const [user, ingress] = key.split('/');
            const owner = world.users.find(candidate => candidate.id === user);
            return owner && world.chains.some(chain => chain.id === ingress)
              ? [{ tenant: owner.tenant, user, ingress }]
              : [];
          }),
        },
      ],
    },
  } as unknown as ConsoleSnapshot;
}

function inflow(period: SiteUsagePeriod): NodeInflowUsage {
  const agg = aggregate(built, period),
    chainList = views(period),
    input = inflowOf(world, agg, chainList);
  return {
    period,
    range_start_unix_secs: agg.bounds.start,
    range_end_unix_secs: NOW,
    period_end_unix_secs: agg.bounds.end,
    latest_report_unix_secs: NOW,
    rows: input.rows.flatMap(user =>
      chainList.flatMap(view => {
        const cell = user.cells.get(view.id);
        if (!cell?.total) return [];
        return [
          {
            tenant_id: user.tenant,
            user_id: user.id,
            source: view.kind === 'chain' ? 'chain' : view.kind === 'fwd' ? 'forwarding' : 'unattributed',
            app_id: view.kind === 'chain' ? 'app' : null,
            chain_id: view.kind === 'chain' ? view.id : null,
            ingress_id: null,
            uplink_bytes: Math.round(cell.up),
            downlink_bytes: Math.round(cell.down),
            has_gap: cell.gap,
          },
        ];
      }),
    ),
    idle_users: input.idle.map(user => ({ tenant_id: user.tenant, user_id: user.id })),
  } as NodeInflowUsage;
}

/* ── 来源：每位用户几台设备，各自的网络归属由本地 GeoIP 给出 ── */

interface Device {
  ip: string;
  country: string | null;
  operator: NetworkOperator | null;
}
const hex = (random: () => number, digits = 4) =>
  Math.floor(random() * 16 ** digits)
    .toString(16)
    .replace(/^0+(?=.)/, '');
/** Household and office networks, a phone on IPv6, one trip abroad and one address the local database misses. */
function devicesOf(user: string): Device[] {
  const random = mulberry(hash(`devices/${user}`));
  const octet = () => 2 + Math.floor(random() * 250);
  return [
    { ip: `203.0.113.${octet()}`, country: 'CN', operator: 'chinanet' },
    {
      ip: `2001:db8:4c1a:${hex(random)}:${hex(random)}:${hex(random)}:${hex(random)}:${hex(random)}`,
      country: 'CN',
      operator: 'cmcc',
    },
    { ip: `198.51.100.${octet()}`, country: 'CN', operator: 'unicom' },
    { ip: `2001:db8:abcd:${hex(random)}::${hex(random, 2)}`, country: 'CN', operator: 'chinanet' },
    { ip: `192.0.2.${octet()}`, country: 'JP', operator: null },
    { ip: `198.51.100.${octet()}`, country: 'US', operator: null },
    { ip: `203.0.113.${octet()}`, country: null, operator: null },
  ];
}
/** A phone that keeps reconnecting: a fresh mobile /64 or carrier-NAT address almost every session. */
function mobileDevices(user: string, count: number): Device[] {
  const random = mulberry(hash(`mobile/${user}`));
  return Array.from({ length: count }, (_, index) => {
    if (index % 3 === 2)
      return {
        ip: `198.51.100.${2 + Math.floor(random() * 250)}`,
        country: 'CN',
        operator: random() < 0.7 ? 'unicom' : 'cmcc',
      } as Device;
    return {
      ip: `2001:db8:${hex(random)}:${hex(random)}:${hex(random)}:${hex(random)}:${hex(random)}:${hex(random)}`,
      country: 'CN',
      operator: 'cmcc',
    } as Device;
  });
}

/** Which devices carry a chain, and what part of the chain's bytes each one has. */
const SPLITS: [device: number, share: number][][] = [
  [
    [0, 0.58],
    [1, 0.24],
    [2, 0.1],
  ],
  [
    [1, 0.62],
    [4, 0.38],
  ],
  [[2, 1]],
  [
    [3, 0.7],
    [6, 0.3],
  ],
  [[5, 1]],
];

type Row = NodeInflowDetailRow;
const scaled = (row: Row, share: number, ip: string | null): Row => ({
  ...row,
  uplink_bytes: Math.round(row.uplink_bytes * share),
  downlink_bytes: Math.round(row.downlink_bytes * share),
  has_gap: false,
  source_ip: ip,
});
function rest(row: Row, parts: Row[]): Row {
  return {
    ...row,
    uplink_bytes: row.uplink_bytes - parts.reduce((sum, part) => sum + part.uplink_bytes, 0),
    downlink_bytes: row.downlink_bytes - parts.reduce((sum, part) => sum + part.downlink_bytes, 0),
    source_ip: null,
  };
}

function details(period: SiteUsagePeriod, tenant: string, user: string, range: [number, number] | null) {
  const usage = inflow(period);
  const ledger = usage.rows.filter(row => row.user_id === user && row.tenant_id === tenant);
  const chains = ledger.filter(row => row.source === 'chain');
  const devices = devicesOf(user);
  const located: Device[] = [];
  let rows: Row[] = [];
  if (detailScene === 'single') {
    const top = chains[0] ?? ledger[0];
    if (top) {
      rows = [{ ...top, source_ip: top.source === 'chain' ? devices[0].ip : null }];
      located.push(devices[0]);
    }
  } else if (detailScene !== 'empty') {
    chains.forEach((row, index) => {
      if (detailScene === 'mobile' && index === 0) {
        const phone = mobileDevices(user, 31);
        // Heavy head, long tail: a few long sessions and many short reconnects.
        const weights = phone.map((_, i) => 1 / (i + 1) ** 1.25);
        const sum = weights.reduce((a, b) => a + b, 0);
        const parts = phone.map((device, i) =>
          scaled({ ...row, source_ip: null }, (weights[i] / sum) * 0.94, device.ip),
        );
        located.push(...phone);
        rows.push(...parts, rest({ ...row, source_ip: null }, parts));
        return;
      }
      const split = SPLITS[index % SPLITS.length];
      const base = { ...row, source_ip: null };
      const parts = split.map(([device, share]) => {
        located.push(devices[device]);
        return scaled(base, share, devices[device].ip);
      });
      if (index === 0) {
        // The top chain keeps the bytes recorded before source observation started.
        rows.push(...parts, rest(base, parts));
        return;
      }
      // Other chains are fully observed: the last source takes the rounding remainder.
      const last = parts.length - 1;
      rows.push(...parts.slice(0, last), { ...rest(base, parts.slice(0, last)), source_ip: parts[last].source_ip });
    });
    rows.push(...ledger.filter(row => row.source !== 'chain').map(row => ({ ...row, source_ip: null }) as Row));
  }
  const accounting = {
    up: ledger.reduce((sum, row) => sum + row.uplink_bytes, 0),
    down: ledger.reduce((sum, row) => sum + row.downlink_bytes, 0),
  };
  let mismatch = false;
  if (detailScene === 'degraded') {
    // Source observation and billing disagree on a chain: residuals merge into one unattributed row,
    // and observation reports dropped counters for the window.
    const observed = rows.filter(row => row.source_ip !== null);
    const forwarding = rows.filter(row => row.source === 'forwarding');
    const authenticated = ledger.filter(row => row.source !== 'forwarding');
    const up = authenticated.reduce((sum, row) => sum + row.uplink_bytes, 0);
    const down = authenticated.reduce((sum, row) => sum + row.downlink_bytes, 0);
    const shrink = observed.map(row => ({
      ...row,
      uplink_bytes: Math.round(row.uplink_bytes * 0.86),
      downlink_bytes: Math.round(row.downlink_bytes * 0.86),
    }));
    rows = [
      ...shrink,
      ...forwarding,
      {
        tenant_id: tenant,
        user_id: user,
        source: 'unattributed',
        app_id: null,
        chain_id: null,
        ingress_id: null,
        uplink_bytes: up - shrink.reduce((sum, row) => sum + row.uplink_bytes, 0),
        downlink_bytes: down - shrink.reduce((sum, row) => sum + row.downlink_bytes, 0),
        has_gap: true,
        source_ip: null,
      },
    ];
    mismatch = true;
  }
  const seen = new Set<string>();
  const unique = located.filter(device => !seen.has(device.ip) && seen.add(device.ip));
  return {
    range_start_unix_secs: range ? range[0] : usage.range_start_unix_secs,
    range_end_unix_secs: range ? range[1] : usage.range_end_unix_secs,
    observed_since_unix_secs: NOW - 9 * 86_400,
    latest_report_unix_secs: NOW,
    limited: detailScene === 'degraded',
    accounting_mismatch: mismatch,
    accounting_uplink_bytes: accounting.up,
    accounting_downlink_bytes: accounting.down,
    rows: rows.filter(row => row.uplink_bytes + row.downlink_bytes > 0),
    // Proposed: the Console decorates inflow-details with the same local GeoIP lookup as the 在线设备 card.
    source_countries: unique.flatMap(device => (device.country ? [{ ip: device.ip, country: device.country }] : [])),
    source_operators: unique.flatMap(device => (device.operator ? [{ ip: device.ip, operator: device.operator }] : [])),
  } satisfies NodeInflowDetails & Record<string, unknown>;
}

/** `user` narrows the machine to one user, as the outflow focus mockup proposes for the Console. */
function sites(period: SiteUsagePeriod, user: string | null = null): NodeSiteUsage {
  const scope = user ? { ...built, world: { ...world, users: world.users.filter(entry => entry.id === user) } } : built;
  const agg = aggregate(built, period),
    rows = siteRowsOf(scope, agg);
  return {
    period,
    collection_enabled: true,
    catalog_version: 1,
    range_start_unix_secs: agg.bounds.start,
    range_end_unix_secs: NOW,
    period_end_unix_secs: agg.bounds.end,
    bucket_secs: agg.bounds.bucket,
    reporting_nodes: 1,
    expected_nodes: 1,
    limited: false,
    latest_report_unix_secs: NOW,
    observed_since_unix_secs: agg.bounds.start,
    rows: rows.map(row => ({
      site_id: row.id,
      name: row.name,
      uplink_bytes: Math.round(row.up),
      downlink_bytes: Math.round(row.down),
    })),
    series: rows.map(row => ({
      site_id: row.id,
      points: row.values.map(
        (value, i) => [agg.bounds.start + i * agg.bounds.bucket, 0, Math.round(value)] as [number, number, number],
      ),
    })),
    top_users: rows.map(row => ({
      site_id: row.id,
      users: row.users.slice(0, 4).map(entry => ({
        tenant_id: world.users.find(candidate => candidate.id === entry.id)!.tenant,
        user_id: entry.id,
        uplink_bytes: 0,
        downlink_bytes: Math.round(entry.total),
      })),
      other_users: Math.max(0, row.users.length - 4),
      other_uplink_bytes: 0,
      other_downlink_bytes: Math.round(row.users.slice(4).reduce((sum, entry) => sum + entry.total, 0)),
    })),
  } as NodeSiteUsage;
}

function trafficHistory(period: SiteUsagePeriod, from: number | null, until: number | null): NodeTrafficHistory {
  const agg = aggregate(built, period);
  const start = from ?? agg.bounds.start,
    end = until ?? NOW;
  const step = from !== null ? 30 : { day: 300, week: 900, month: 3600 }[period];
  const points: NodeTrafficHistory['points'] = [];
  let recorded = 0;
  const lanes = [...agg.lanes.values()];
  for (let at = start; at < end; at += step) {
    const index = Math.floor((at - agg.bounds.start) / agg.bounds.bucket);
    const bucketEnd = Math.min(at + step, end);
    if (!agg.buckets[index] || agg.buckets[index].observed === 'none') {
      points.push([bucketEnd, null, null]);
      continue;
    }
    const up = lanes.reduce((sum, lane) => sum + (lane.up[index] ?? 0), 0) / agg.bounds.bucket;
    const down = lanes.reduce((sum, lane) => sum + (lane.down[index] ?? 0), 0) / agg.bounds.bucket;
    recorded += (up + down) * (bucketEnd - at);
    points.push([bucketEnd, up, down]);
  }
  return {
    range_start_unix_secs: start,
    range_end_unix_secs: end,
    bucket_secs: step,
    points,
    recorded_bytes: recorded,
    has_gap: agg.gapHours > 0,
    has_unplaced: false,
  };
}

function sample(): NodeRealtimeEvent {
  return {
    node_id: NODE_ID,
    received_at_unix_millis: Date.now(),
    sample: {
      sampled_at_unix_millis: Date.now(),
      user_traffic: {
        state: 'ready',
        all_users: true,
        forwarding_supported: true,
        users: liveRates(built, hour, 0).flatMap(user =>
          user.chains.map(chain => ({
            tenant_id: user.tenant,
            user_id: user.id,
            ...(chain.col === 'fwd' ? { forwarding_rule_id: 1 } : { ingress_id: chain.col }),
            uplink_bytes_per_sec: chain.up,
            downlink_bytes_per_sec: chain.down,
          })),
        ),
      },
    },
  } as NodeRealtimeEvent;
}

/** The mockup is about the details dialog, so the live card behind it stays on one sample. */
class FixtureEvents extends EventTarget {
  onerror = null;
  timer: ReturnType<typeof setInterval>;
  constructor() {
    super();
    setTimeout(() => this.dispatchEvent(new MessageEvent('sample', { data: JSON.stringify(sample()) })), 20);
    this.timer = setInterval(
      () => this.dispatchEvent(new MessageEvent('sample', { data: JSON.stringify(sample()) })),
      1000,
    );
  }
  close() {
    clearInterval(this.timer);
  }
}

export function installBackend() {
  Object.defineProperty(window, 'EventSource', { value: FixtureEvents, configurable: true });
  const nativeFetch = window.fetch;
  window.fetch = async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : String(input), location.href);
    const period = (url.searchParams.get('period') ?? 'day') as SiteUsagePeriod;
    const from = Number(url.searchParams.get('start_unix_secs'));
    const until = Number(url.searchParams.get('end_unix_secs'));
    const custom = from > 0 && until > from;
    if (url.pathname.endsWith('/inflow-usage')) {
      const data = inflow(period);
      return Response.json(
        custom
          ? { ...data, range_start_unix_secs: from, range_end_unix_secs: until, period_end_unix_secs: until }
          : data,
      );
    }
    if (url.pathname.endsWith('/inflow-details')) {
      if (detailScene === 'error') return Response.json({ error: 'database unavailable' }, { status: 503 });
      const tenant = url.searchParams.get('tenant_id') ?? '';
      const user = url.searchParams.get('user_id') ?? '';
      return Response.json(details(period, tenant, user, custom ? [from, until] : null));
    }
    if (url.pathname.endsWith('/site-usage')) {
      const user = url.searchParams.get('user_id');
      // A narrowed read takes a moment longer, so the mockup shows the switching state.
      if (user) await new Promise(resolve => setTimeout(resolve, 180));
      return Response.json(sites(period, user));
    }
    if (url.pathname.endsWith('/other-domains') || url.pathname.endsWith('/sniffing-failed-targets'))
      return Response.json({
        collection_enabled: false,
        top_n: 128,
        range_start_unix_secs: NOW - 86400,
        range_end_unix_secs: NOW,
        reporting_nodes: 0,
        expected_nodes: 1,
        limited: false,
        observed_since_unix_secs: null,
        latest_report_unix_secs: null,
        rows: [],
      });
    if (url.pathname.endsWith('/traffic-history'))
      return Response.json(trafficHistory(period, custom ? from : null, custom ? until : null));
    return nativeFetch(input, init);
  };
}
