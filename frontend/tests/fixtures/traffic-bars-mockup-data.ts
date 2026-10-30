// Synthetic Console for the traffic bar chart mockup (traffic-bars-mockup.html). Never imported by the app.
// Same scenes and numbers as node-traffic-acceptance.tsx; history windows get per-window noise so the
// 5-minute bars look like real traffic instead of hourly steps.
import type {
  ConsoleSnapshot,
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
  gauss,
  hash,
  inflowOf,
  liveHistory,
  liveRates,
  mulberry,
  NOW,
  SCENES,
  siteRowsOf,
  type Built,
  type LiveHistory,
  type SceneKey,
  type World,
} from './node-traffic-mockup-data';

export const NODE_ID = 'n1';
export const NODE_NAME = '香港测试机器';

export const HISTORY_CASES = [
  { value: 'normal', label: '自然流量' },
  { value: 'duplex', label: '双向高流量' },
  { value: 'units', label: '单位跨档' },
  { value: 'single', label: '单个窗口' },
] as const;
export type HistoryCase = (typeof HISTORY_CASES)[number]['value'];
let historyCase: HistoryCase = 'normal';
const historySteps = new Map<string, number>();
const historyKey = (start: number, end: number) => `${start}/${end}`;

export function selectHistoryCase(next: HistoryCase) {
  historyCase = next;
}

/** The response's cadence stays available when a period is revisited from the query cache. */
export function historyBucketMillis(startMillis: number, endMillis: number): number {
  const step = historySteps.get(historyKey(startMillis / 1000, endMillis / 1000));
  if (step === undefined) throw new Error('Mockup history chart has no matching window metadata');
  return step * 1000;
}

const live: { world: World; built: Built; hour: LiveHistory; scene: SceneKey } = (() => {
  const world = SCENES.typical();
  const built = build(world);
  return { world, built, hour: liveHistory(built), scene: 'typical' };
})();

export function selectScene(scene: SceneKey) {
  if (live.scene === scene) return;
  live.world = SCENES[scene]();
  live.built = build(live.world);
  live.hour = liveHistory(live.built);
  live.scene = scene;
}

export function snapshotOf(): ConsoleSnapshot {
  const { world } = live;
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
  const { world, built } = live;
  const agg = aggregate(built, period),
    views = chainViews(world, agg, aggregate(built, 'month')),
    input = inflowOf(world, agg, views);
  return {
    period,
    range_start_unix_secs: agg.bounds.start,
    range_end_unix_secs: NOW,
    period_end_unix_secs: agg.bounds.end,
    latest_report_unix_secs: NOW,
    rows: input.rows.flatMap(user =>
      views.flatMap(view => {
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

function sites(period: SiteUsagePeriod): NodeSiteUsage {
  const { world, built } = live;
  const agg = aggregate(built, period),
    rows = siteRowsOf(built, agg);
  return {
    period,
    collection_enabled: world.sites,
    catalog_version: 1,
    range_start_unix_secs: agg.bounds.start,
    range_end_unix_secs: NOW,
    period_end_unix_secs: agg.bounds.end,
    bucket_secs: agg.bounds.bucket,
    reporting_nodes: world.sites ? 1 : 0,
    expected_nodes: 1,
    limited: false,
    latest_report_unix_secs: world.sites ? NOW : null,
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
      users: row.users.slice(0, 4).map(user => ({
        tenant_id: world.users.find(u => u.id === user.id)!.tenant,
        user_id: user.id,
        uplink_bytes: 0,
        downlink_bytes: Math.round(user.total),
      })),
      other_users: Math.max(0, row.users.length - 4),
      other_uplink_bytes: 0,
      other_downlink_bytes: Math.round(row.users.slice(4).reduce((sum, user) => sum + user.total, 0)),
    })),
  } as NodeSiteUsage;
}

function trafficHistory(period: SiteUsagePeriod, from: number | null, until: number | null): NodeTrafficHistory {
  const { built, scene } = live;
  const agg = aggregate(built, period);
  const start = from ?? agg.bounds.start,
    end = until ?? NOW;
  const step = from !== null ? 30 : { day: 300, week: 900, month: 3600 }[period];
  const points: NodeTrafficHistory['points'] = [];
  let recorded = 0;
  // Mean-one log-normal noise per window, fixed per scene and window start.
  const sigma = 0.45;
  for (let at = start; at < end; at += step) {
    const index = Math.floor((at - agg.bounds.start) / agg.bounds.bucket);
    const bucketEnd = Math.min(at + step, end);
    if (!agg.buckets[index] || agg.buckets[index].observed === 'none') {
      points.push([bucketEnd, null, null]);
      continue;
    }
    const random = mulberry(hash(`${scene}/history/${at}`));
    const noise = Math.exp(gauss(random) * sigma - (sigma * sigma) / 2);
    const lanes = [...agg.lanes.values()];
    // Symmetric traffic exposes clipped stacks; 1.6 + 1.6 Gbit/s also crosses the axis-unit boundary.
    const reviewRate = historyCase === 'duplex' ? 80_000_000 / 8 : historyCase === 'units' ? 1_600_000_000 / 8 : null;
    const up = reviewRate ?? (lanes.reduce((sum, lane) => sum + (lane.up[index] ?? 0), 0) / agg.bounds.bucket) * noise;
    const down =
      reviewRate ?? (lanes.reduce((sum, lane) => sum + (lane.down[index] ?? 0), 0) / agg.bounds.bucket) * noise;
    recorded += (up + down) * (bucketEnd - at);
    points.push([bucketEnd, up, down]);
  }
  if (historyCase === 'single') {
    const kept = points.find(point => point[1] !== null && point[2] !== null);
    points.splice(0, points.length, ...(kept ? [kept] : []));
    recorded = kept ? ((kept[1] ?? 0) + (kept[2] ?? 0)) * Math.min(step, kept[0] - start) : 0;
  }
  // There is one chart and a small set of review ranges; cap metadata even during repeated brushing.
  const key = historyKey(start, end);
  historySteps.delete(key);
  historySteps.set(key, step);
  if (historySteps.size > 16) historySteps.delete(historySteps.keys().next().value!);
  return {
    range_start_unix_secs: start,
    range_end_unix_secs: end,
    bucket_secs: step,
    points,
    recorded_bytes: recorded,
    has_gap: historyCase === 'single' || agg.gapHours > 0,
    has_unplaced: false,
  };
}

function sample(second: number): NodeRealtimeEvent {
  return {
    node_id: NODE_ID,
    received_at_unix_millis: Date.now(),
    sample: {
      sampled_at_unix_millis: Date.now(),
      user_traffic: {
        state: 'ready',
        all_users: true,
        forwarding_supported: true,
        users: liveRates(live.built, live.hour, second).flatMap(user =>
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

class FixtureEvents extends EventTarget {
  onerror = null;
  timer: ReturnType<typeof setInterval>;
  constructor() {
    super();
    // The mockup is about the bar charts, so the live rates stay on one sample.
    setTimeout(() => this.dispatchEvent(new MessageEvent('sample', { data: JSON.stringify(sample(0)) })), 20);
    this.timer = setInterval(
      () => this.dispatchEvent(new MessageEvent('sample', { data: JSON.stringify(sample(0)) })),
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
    const detail = {
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
    };
    if (url.pathname.endsWith('/inflow-usage')) {
      const data = inflow(period);
      return Response.json(
        custom
          ? { ...data, range_start_unix_secs: from, range_end_unix_secs: until, period_end_unix_secs: until }
          : data,
      );
    }
    if (url.pathname.endsWith('/inflow-details')) {
      const data = inflow(period);
      const user = url.searchParams.get('user_id');
      const tenant = url.searchParams.get('tenant_id');
      const rows = data.rows.filter(row => row.user_id === user && row.tenant_id === tenant);
      return Response.json({
        range_start_unix_secs: custom ? from : data.range_start_unix_secs,
        range_end_unix_secs: custom ? until : data.range_end_unix_secs,
        observed_since_unix_secs: NOW - 3600,
        latest_report_unix_secs: NOW,
        limited: false,
        accounting_mismatch: false,
        accounting_uplink_bytes: rows.reduce((sum, row) => sum + row.uplink_bytes, 0),
        accounting_downlink_bytes: rows.reduce((sum, row) => sum + row.downlink_bytes, 0),
        rows: rows.map((row, i) => ({ ...row, source_ip: row.source === 'chain' ? `192.0.2.${i + 1}` : null })),
      });
    }
    if (url.pathname.endsWith('/site-usage')) return Response.json(sites(period));
    if (url.pathname.endsWith('/other-domains') || url.pathname.endsWith('/sniffing-failed-targets'))
      return Response.json(detail);
    if (url.pathname.endsWith('/traffic-history'))
      return Response.json(trafficHistory(period, custom ? from : null, custom ? until : null));
    return nativeFetch(input, init);
  };
}
