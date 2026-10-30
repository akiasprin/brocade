// Synthetic chains and users for the list page header mockup (线路 / 用户). Never imported by the app.
// Machines come from the machine list mockup so chain paths name the same fleet; addresses are
// documentation ranges and user ids are made up.
import {
  DEFAULT_HOP_MUX,
  type ConsoleSnapshot,
  type E2eProbeItem,
  type E2eProbeStatus,
  type Rule,
  type SnapshotApp,
  type SnapshotIngress,
  type SnapshotStep,
  type UserAppQuota,
  type UserCycleUsage,
  type UserListItem,
  type UserOnlineSource,
  type UserPresenceList,
} from '../../src/api';
import type { ModelOp } from '../../src/draft';
import { SCENES, type Machine } from './node-list-mockup-data';

export const TENANT = 'platform';
const GiB = 1024 ** 3;

export const MACHINES: Machine[] = SCENES.typical.machines();
const machineOf = (id: string) => MACHINES.find(machine => machine.id === id)!;

/* ══ 线路 ══ */

type ProbeShape = 'ok' | 'slow' | 'odd' | 'down' | 'none';

interface ChainSpec {
  id: string;
  name: string;
  country: string;
  /** Machine ids from the entry to the exit. */
  path: string[];
  /** false = 缺接入面. */
  ingress?: boolean;
  hy2?: boolean;
  xhttp?: boolean;
  probe: ProbeShape;
  /** Typical first-byte time in ms. */
  ms?: number;
  users: string[];
}

interface GroupSpec {
  id: string;
  label: string;
  chains: ChainSpec[];
}

const EVERYONE = ['alice', 'bob', 'carol', 'dave', 'frank', 'grace', 'heidi', 'ivan', 'kim', 'leo', 'mia'];

// prettier-ignore
const GROUPS: GroupSpec[] = [
  {
    id: 'apac',
    label: '亚太精品',
    chains: [
      { id: 'hk-direct', name: '香港 直出', country: 'HK', path: ['hk-hkix'], probe: 'ok', ms: 96, hy2: true, users: EVERYONE },
      { id: 'jp-iij', name: '东京 IIJ', country: 'JP', path: ['hk-cmi', 'jp-iij'], probe: 'ok', ms: 162, users: EVERYONE },
      { id: 'jp-sb', name: '东京 SoftBank', country: 'JP', path: ['hk-cmi', 'jp-sb'], probe: 'down', ms: 171, users: ['alice', 'bob', 'ivan', 'mia'] },
      { id: 'tw-hinet', name: '台北 HiNet', country: 'TW', path: ['hk-hkix', 'tw-hinet'], probe: 'ok', ms: 128, users: ['alice', 'bob', 'dave', 'ivan'] },
      { id: 'sg-do', name: '新加坡', country: 'SG', path: ['hk-hkix', 'sg-dp'], probe: 'ok', ms: 214, users: ['alice', 'frank', 'leo'] },
      { id: 'kr-kt', name: '首尔 KT', country: 'KR', path: ['jp-bgp', 'kr-kt'], probe: 'ok', ms: 187, users: ['bob', 'mia'] },
    ],
  },
  {
    id: 'cn2',
    label: '三网优化',
    chains: [
      { id: 'us-gia', name: '洛杉矶 CN2 GIA', country: 'US', path: ['hk-bgp', 'us-gia'], probe: 'ok', ms: 241, users: EVERYONE },
      { id: 'us-9929', name: '洛杉矶 9929', country: 'US', path: ['hk-bgp', 'us-9929'], probe: 'down', ms: 236, users: ['alice', 'dave', 'grace'] },
      { id: 'us-cmin2', name: '洛杉矶 CMIN2', country: 'US', path: ['hk-cmi', 'us-cmin2'], probe: 'slow', ms: 438, users: ['bob', 'ivan', 'leo'] },
      { id: 'mo-ctm', name: '澳门 CTM', country: 'MO', path: ['mo-ctm'], probe: 'ok', ms: 74, xhttp: true, users: ['alice', 'kim'] },
    ],
  },
  {
    id: 'west',
    label: '欧美标准',
    chains: [
      { id: 'us-sjc', name: '圣何塞', country: 'US', path: ['us-sjc'], probe: 'ok', ms: 268, users: ['alice', 'frank', 'mia'] },
      { id: 'us-sea', name: '西雅图', country: 'US', path: ['us-sea'], probe: 'none', users: [] },
      { id: 'de-fra', name: '法兰克福', country: 'DE', path: ['jp-iij', 'de-fra'], probe: 'ok', ms: 286, users: ['bob', 'leo'] },
      { id: 'gb-lon', name: '伦敦 家宽', country: 'GB', path: ['hk-hkix', 'gb-lon'], probe: 'odd', ms: 252, users: ['alice', 'ivan'] },
      { id: 'kr-old', name: '首尔 旧线', country: 'KR', path: ['hk-hkix', 'kr-old'], probe: 'none', users: ['carol'] },
    ],
  },
  {
    id: 'lab',
    label: '测试',
    chains: [{ id: 'nl-ams', name: '阿姆斯特丹', country: 'NL', path: ['nl-ams'], ingress: false, probe: 'none', users: [] }],
  },
];

const forward = (to: string): Rule =>
  ({
    m: { t: 'any' },
    a: { t: 'forward', to, dial: { t: 'overlay' }, pool: { t: 'mux', v: { ...DEFAULT_HOP_MUX } } },
  }) as Rule;
const egress = { m: { t: 'any' }, a: { t: 'egress', send_through: null } } as Rule;

const GUARD = {
  no_private: true,
  no_bittorrent: true,
  no_mail: true,
  no_udp_amplification: true,
  tcp_and_quic_only: false,
  cn_source_block: 'off',
};

function ingressOf(chain: ChainSpec, index: number): SnapshotIngress {
  const vless = chain.xhttp
    ? { kind: 'vless-reality-xhttp', xhttp: { path: '/v', mode: 'auto' } }
    : { kind: 'vless-reality', dest: 'www.example.com:443', server_names: ['www.example.com'], fingerprint: 'chrome' };
  return {
    id: `in-${chain.id}`,
    chain: chain.id,
    node: chain.path[0],
    bind: '0.0.0.0',
    port: 8443 + index,
    projection: {},
    guard: GUARD,
    identity: { public_key: 'preview', short_ids: ['0123abcd'] },
    wires: {
      vless,
      ...(chain.hy2 && {
        hysteria2: { port: 9443 + index, congestion: 'bbr', obfs: null, hop: null, up_mbps: null, down_mbps: null },
      }),
    },
  } as unknown as SnapshotIngress;
}

function appOf(group: GroupSpec, offset: number): SnapshotApp {
  const ingresses = group.chains
    .map((chain, index) => (chain.ingress === false ? null : ingressOf(chain, offset + index)))
    .filter((ingress): ingress is SnapshotIngress => ingress !== null);
  const steps: SnapshotStep[] = group.chains.flatMap(chain =>
    chain.path.map((node, index) => ({
      chain: chain.id,
      node,
      accept: index === 0 ? null : { uuid: `accept-${chain.id}-${node}`, label: `${chain.name} 中继` },
      hop_in: index === 0 ? null : { port: 22000 + offset + index, security: { t: 'none' } },
      rules: [index === chain.path.length - 1 ? egress : forward(chain.path[index + 1])],
    })),
  );
  return {
    id: group.id,
    label: group.label,
    chains: group.chains.map(chain => ({
      id: chain.id,
      tenant: TENANT,
      name: chain.name,
      subscription_country: chain.country,
    })),
    steps,
    ingresses,
    fronts: [],
    grants: group.chains.flatMap(chain =>
      chain.ingress === false ? [] : chain.users.map(user => ({ tenant: TENANT, user, ingress: `in-${chain.id}` })),
    ),
  };
}

export const APPS: SnapshotApp[] = GROUPS.map((group, index) => appOf(group, index * 10));

export const chainSnapshot = (masked: boolean): ConsoleSnapshot =>
  ({
    snapshot: {
      revision: 41,
      settings: { relay_mux: DEFAULT_HOP_MUX },
      external_outbounds: [],
      apps: APPS,
      nodes: MACHINES.map(machine => ({ id: machine.id, tenant: TENANT, name: machine.name, overlay: true })),
    },
    node_egress_dns: [],
    redacted: masked,
  }) as unknown as ConsoleSnapshot;

/** The draft preview: applies the list page's own draft ops (删链、新建分组、改名、排序) to the snapshot.
 *  Drafts persist in localStorage, so without this a deleted chain would break the page until discarded. */
export function previewSnapshot(base: ConsoleSnapshot, ops: ModelOp[]): ConsoleSnapshot {
  const next = structuredClone(base);
  const apps = next.snapshot.apps;
  const order = <T>(items: T[], ids: string[], idOf: (item: T) => string) =>
    items.sort((a, b) => ids.indexOf(idOf(a)) - ids.indexOf(idOf(b)));
  for (const op of ops) {
    if (op.op === 'create_app')
      apps.push({ id: op.app.id, label: op.app.label, chains: [], steps: [], ingresses: [], fronts: [], grants: [] });
    if (op.op === 'upsert_app') {
      const app = apps.find(item => item.id === op.app.id);
      if (app) app.label = op.app.label;
    }
    if (op.op === 'reorder_apps') order(apps, op.ids, app => app.id);
    if (op.op === 'move_chain') {
      const source = apps.find(item => item.id === op.app_id);
      const target = apps.find(item => item.id === op.target_app_id);
      if (!source || !target || source === target) continue;
      const ids = new Set(source.ingresses.filter(item => item.chain === op.chain_id).map(item => item.id));
      target.chains.push(...source.chains.filter(item => item.id === op.chain_id));
      target.ingresses.push(...source.ingresses.filter(item => item.chain === op.chain_id));
      target.steps.push(...source.steps.filter(item => item.chain === op.chain_id));
      target.grants.push(...source.grants.filter(item => ids.has(item.ingress)));
      source.chains = source.chains.filter(item => item.id !== op.chain_id);
      source.ingresses = source.ingresses.filter(item => item.chain !== op.chain_id);
      source.steps = source.steps.filter(item => item.chain !== op.chain_id);
      source.grants = source.grants.filter(item => !ids.has(item.ingress));
    }
    if (op.op === 'reorder_chains') {
      const app = apps.find(item => item.id === op.app_id);
      if (app) order(app.chains, op.ids, chain => chain.id);
    }
    if (op.op === 'delete_chain') {
      const app = apps.find(item => item.id === op.app_id);
      if (!app) continue;
      const removed = new Set(app.ingresses.filter(item => item.chain === op.chain_id).map(item => item.id));
      app.chains = app.chains.filter(chain => chain.id !== op.chain_id);
      app.ingresses = app.ingresses.filter(item => item.chain !== op.chain_id);
      app.steps = app.steps.filter(step => step.chain !== op.chain_id);
      app.grants = app.grants.filter(grant => !removed.has(grant.ingress));
    }
  }
  return next;
}

/** Deterministic noise in [0, 1). */
const noise = (seed: number) => {
  const x = Math.sin(seed * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
};
const hash = (text: string) => [...text].reduce((sum, char) => (sum * 31 + char.charCodeAt(0)) % 9973, 7);

const PROBE_STEP_SECS = 300;
const PROBE_SAMPLES = 72;

function probeOf(group: GroupSpec, chain: ChainSpec, nowSecs: number): E2eProbeItem | null {
  if (chain.probe === 'none') return null;
  const seed = hash(chain.id);
  const last = Math.floor(nowSecs / 60) * 60 - 20;
  const base = chain.ms ?? 180;
  // 故障从近一小时开始：曲线前段正常，末段是红色虚线。
  const failedFrom = PROBE_SAMPLES - 11;
  const status: E2eProbeStatus[] = [];
  const ttfb: Array<number | null> = [];
  const at: number[] = [];
  for (let index = 0; index < PROBE_SAMPLES; index += 1) {
    at.push(last - (PROBE_SAMPLES - 1 - index) * PROBE_STEP_SECS);
    if (chain.probe === 'down' && index >= failedFrom) {
      status.push(
        chain.path.length > 1 && machineOf(chain.path.at(-1)!).state === 'offline' ? 'timeout' : 'chain-broken',
      );
      ttfb.push(null);
      continue;
    }
    // 慢波加小幅抖动，偶尔一次突起：自动缩放的纵轴会把单根尖刺拉满整格，抖动让曲线像真实读数。
    const bump = noise(seed + index) > 0.97 ? 0.08 : 0;
    const wave =
      0.06 * Math.sin(index / 10 + seed) +
      0.035 * Math.sin(index / 4.3 + seed * 2) +
      0.035 * (noise(seed * 3 + index) - 0.5) +
      bump;
    status.push('ok');
    ttfb.push(Math.round(base * (1 + wave)));
  }
  const exit = machineOf(chain.path.at(-1)!);
  const entry = machineOf(chain.path[0]);
  const odd = chain.probe === 'odd';
  const latestStatus = status.at(-1)!;
  return {
    app_id: group.id,
    chain_id: chain.id,
    chain_name: chain.name,
    node_id: chain.path[0],
    status: latestStatus,
    ttfb_ms: ttfb.at(-1) ?? null,
    exit_ip: latestStatus === 'ok' ? (odd ? entry.ipv4 : (exit.ipv4 ?? '192.0.2.200')) : null,
    exit_loc: latestStatus === 'ok' ? (odd ? entry.country : exit.country) : null,
    exit_verdict: latestStatus !== 'ok' ? 'unknown' : odd ? 'mismatch' : 'match',
    detail:
      latestStatus === 'timeout'
        ? '出口机器 20 秒内没有回应'
        : latestStatus === 'chain-broken'
          ? '入口握手成功，第二跳 WireGuard 不通'
          : null,
    probed_at: new Date(last * 1000).toISOString(),
    samples: { probed_at_unix_secs: at, status, ttfb_ms: ttfb },
  };
}

export const e2eProbes = (nowSecs: number) => ({
  chains: GROUPS.flatMap(group => group.chains.flatMap(chain => probeOf(group, chain, nowSecs) ?? [])),
});

/* ══ 用户 ══ */

interface UserSpec {
  id: string;
  test?: boolean;
  disabled?: boolean;
  service?: 'pending' | 'expired';
  /** Cycle usage in GiB per group. */
  used: Partial<Record<string, number>>;
  /** Quota in GiB per group. */
  quota?: Partial<Record<string, number>>;
  throttled?: string[];
  suspended?: string[];
  /** Online source count. */
  online?: number;
}

// prettier-ignore
const USERS: UserSpec[] = [
  { id: 'alice', used: { apac: 182.4, cn2: 96.1, west: 12.7 }, quota: { apac: 500, cn2: 300 }, online: 2 },
  { id: 'bob', used: { apac: 341.2, cn2: 58.3, west: 4.2 }, quota: { apac: 400 }, online: 1 },
  { id: 'carol', test: true, used: { apac: 3.1, west: 0.4 }, quota: { apac: 10 } },
  { id: 'dave', used: { apac: 41.9, cn2: 200.6 }, quota: { cn2: 200 }, suspended: ['cn2'] },
  { id: 'erin', disabled: true, used: {} },
  { id: 'frank', used: { apac: 120.2, west: 30.8 }, quota: { apac: 120 }, throttled: ['apac'], online: 1 },
  { id: 'grace', service: 'pending', used: {} },
  { id: 'heidi', service: 'expired', used: { apac: 18.2 } },
  { id: 'ivan', used: { apac: 76.5, cn2: 31.2, west: 8.9 }, quota: { apac: 300, cn2: 100 }, online: 1 },
  { id: 'judy', used: {} },
  { id: 'kim', test: true, used: { cn2: 1.2 } },
  { id: 'leo', used: { apac: 22.3, cn2: 18.4, west: 41.6 } },
  { id: 'mia', used: { apac: 64.8, west: 9.3 }, quota: { apac: 200 }, online: 1 },
  { id: 'noah', disabled: true, used: { apac: 0.6 } },
];

export const USER_LIST: UserListItem[] = USERS.map((spec, index) => ({
  tenant_id: TENANT,
  id: spec.id,
  uuid: `00000000-0000-4000-8000-${String(index + 1).padStart(12, '0')}`,
  status: spec.disabled ? 'disabled' : 'active',
  account_type: spec.test ? 'test' : 'formal',
  traffic_accounting_mode: 'single',
  traffic_reset_day: 1,
  service_starts_at: spec.service === 'pending' ? '2026-11-01T00:00:00Z' : null,
  service_expires_at: spec.service === 'expired' ? '2026-10-20T00:00:00Z' : null,
  service_state: spec.service ?? 'active',
  login_enabled: true,
  direct_login_enabled: false,
  created_at: '2026-09-01T00:00:00Z',
  created_revision: 3,
}));

const ingressIdsOf = (appId: string) => APPS.find(app => app.id === appId)?.ingresses.map(ingress => ingress.id) ?? [];

export const quotas = (): { quotas: UserAppQuota[] } => ({
  quotas: USERS.flatMap(spec =>
    Object.entries(spec.quota ?? {}).map(([app, gib]) => ({
      tenant_id: TENANT,
      user_id: spec.id,
      app_id: app,
      limit_bytes: Math.round((gib ?? 0) * GiB),
      updated_at: '2026-10-01T00:00:00Z',
      suspended_ingresses: spec.suspended?.includes(app) ? ingressIdsOf(app) : [],
    })),
  ),
});

export const userCycles = (): UserCycleUsage =>
  ({
    periods: USERS.map(spec => ({
      tenant_id: TENANT,
      user_id: spec.id,
      starts_at: '2026-09-30T16:00:00Z',
      ends_at: '2026-10-31T16:00:00Z',
    })),
    forwarding: [],
    traffic_policies: USERS.flatMap(spec =>
      Object.entries(spec.quota ?? {}).map(([app, gib]) => ({
        tenant_id: TENANT,
        user_id: spec.id,
        kind: 'access',
        app_id: app,
        quota_bytes: String(Math.round((gib ?? 0) * GiB)),
        used_bytes: String(Math.round((spec.used[app] ?? 0) * GiB)),
        exhausted: (spec.used[app] ?? 0) >= (gib ?? Infinity),
        suspended: !!spec.suspended?.includes(app),
        rate_bps: spec.throttled?.includes(app) ? 5_000_000 : null,
      })),
    ),
    views: USERS.flatMap(spec =>
      Object.entries(spec.used).map(([app, gib]) => ({
        tenant_id: TENANT,
        user_id: spec.id,
        app_id: app,
        uplink_bytes: Math.round((gib ?? 0) * 0.08 * GiB),
        downlink_bytes: Math.round((gib ?? 0) * 0.92 * GiB),
        has_gap: false,
      })),
    ),
  }) as UserCycleUsage;

const SOURCE_IPS = ['198.51.100.77', '203.0.113.24', '203.0.113.61', '198.51.100.140', '203.0.113.88'];

function onlineSource(ip: string, ingress: string, node: string, minutes: number): UserOnlineSource {
  const now = Date.now();
  const last = new Date(now - 20_000).toISOString();
  const since = new Date(now - minutes * 60_000).toISOString();
  const access = {
    node_id: node,
    ingress_id: ingress,
    protocols: ['vless' as const],
    last_observed_at: last,
    xray_last_seen_at: last,
    online_since_at: since,
    session_uplink_bytes: Math.round(minutes * 0.002 * GiB),
    session_downlink_bytes: Math.round(minutes * 0.03 * GiB),
  };
  return {
    ip,
    first_observed_at: since,
    last_observed_at: last,
    xray_last_seen_at: last,
    online_since_at: since,
    session_uplink_bytes: access.session_uplink_bytes,
    session_downlink_bytes: access.session_downlink_bytes,
    node_ids: [node],
    ingress_ids: [ingress],
    accesses: [access],
  } as UserOnlineSource;
}

export const userPresence = (): UserPresenceList => {
  let next = 0;
  const users = USERS.filter(spec => spec.online).map(spec => ({
    tenant_id: TENANT,
    user_id: spec.id,
    state: 'complete' as const,
    expected_nodes: 3,
    reporting_nodes: 3,
    sources: Array.from({ length: spec.online ?? 0 }, (_, index) => {
      const ip = SOURCE_IPS[next++ % SOURCE_IPS.length];
      return onlineSource(
        ip,
        index === 0 ? 'in-jp-iij' : 'in-hk-direct',
        index === 0 ? 'hk-cmi' : 'hk-hkix',
        40 + index * 90,
      );
    }),
  }));
  return {
    freshness_secs: 120,
    users,
    source_countries: SOURCE_IPS.map(ip => ({ ip, country: 'CN' })),
    source_operators: [],
  };
};

/** The user detail's quota rows: one per line group, as `/users/{tenant}/{user}/traffic-policy` returns them. */
export const trafficPolicies = (userId: string) => {
  const spec = USERS.find(item => item.id === userId);
  return APPS.map(app => {
    const quota = spec?.quota?.[app.id];
    const used = spec?.used[app.id] ?? 0;
    return {
      kind: 'access',
      app_id: app.id,
      policy:
        quota == null
          ? null
          : {
              quota_bytes: String(Math.round(quota * GiB)),
              exhausted_action: spec?.throttled?.includes(app.id)
                ? { kind: 'throttle', rate_bps: 5_000_000 }
                : { kind: 'suspend' },
            },
      used_bytes: String(Math.round(used * GiB)),
      exhausted: quota != null && used >= quota,
      service_available: true,
      unsupported_nodes: [],
      pending_nodes: [],
    };
  });
};
