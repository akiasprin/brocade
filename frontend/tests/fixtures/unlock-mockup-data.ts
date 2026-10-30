// Synthetic machines, chain and unlock-probe jobs for the 解锁检测 mockup. Never imported by the app.
// Machine objects come from the machine-list mockup so the detail head matches production; addresses are
// documentation ranges. Every job keeps the shape the Console returns (UnlockProbeView), including the previous
// results that stay after a run completes.
import type {
  ConsoleSnapshot,
  NodeAgentStateItem,
  NodeUnlockProbe,
  SnapshotIngress,
  SnapshotStep,
  UnlockResult,
  UnlockService,
  YoutubeDiagnostics,
  YoutubePageResult,
} from '../../src/api';
import { DEFAULT_HOP_MUX } from '../../src/api';
import { appIdFromBytes, modelIdPairFromBytes } from '../../src/model-id';
import { agentState, type Machine } from './node-list-mockup-data';

export const nowSecs = () => Math.floor(Date.now() / 1000);

/* ══ 机器与线路 ══ */

// prettier-ignore
export const MACHINES: Machine[] = [
  { id: 'hk-hkix', name: '香港 HKIX', country: 'HK', ipv4: '203.0.113.11', ipv6: '2001:db8:11::1', mbps: 6.2, monthGiB: 612, quotaTiB: 2, ping: [38, 41, 35], state: 'ok' },
  { id: 'jp-iij', name: '东京 IIJ', country: 'JP', ipv4: '198.51.100.51', mbps: 2.1, monthGiB: 900, quotaTiB: 1, ping: [73, 77, 80], state: 'ok' },
  { id: 'us-gia', name: '洛杉矶 CN2 GIA', country: 'US', ipv4: '192.0.2.71', mbps: 0.3, monthGiB: 638, quotaTiB: 2, ping: [166, 164, 170], state: 'ok' },
  { id: 'mo-ctm', name: '澳门 CTM', country: 'MO', ipv4: '203.0.113.31', mbps: 1.3, monthGiB: 15, quotaTiB: 1, ping: [41, 46, 39], state: 'ok' },
];

export const APP_ID = appIdFromBytes(new Uint8Array([7, 3]));
const PAIR = modelIdPairFromBytes(new Uint8Array([7, 3, 1, 9]));
export const CHAIN_ID = PAIR.chainId;
export const CHAIN_NAME = '香港 → 东京';
export const REVISION = 41;

export function machineState(machine: Machine, protocol: number): NodeAgentStateItem {
  return { ...agentState(machine, nowSecs()), agent_protocol_version: protocol };
}

const relay = (to: string) =>
  ({ t: 'forward', to, dial: { t: 'overlay' }, pool: { t: 'mux', v: DEFAULT_HOP_MUX } }) as const;

const ingress: SnapshotIngress = {
  id: PAIR.ingressId,
  chain: CHAIN_ID,
  node: 'hk-hkix',
  bind: '0.0.0.0',
  port: 63001,
  projection: {},
  identity: { public_key: 'mockup-key', short_ids: ['0123456789abcdef'] },
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
    cn_source_block: 'off',
  },
  wires: {
    vless: { kind: 'vless-reality' },
    anytls: { port: 15443, security: 'tls', masquerade: { kind: 'not-found' } },
  },
} as SnapshotIngress;

// 香港入站 → 东京出网；AI 服务在东京再转到洛杉矶。解锁结果里 AI 是 US、其余是 JP，就来自这条分流。
const steps: SnapshotStep[] = [
  {
    chain: CHAIN_ID,
    node: 'hk-hkix',
    accept: null,
    hop_in: null,
    rules: [{ m: { t: 'any' }, a: relay('jp-iij') }],
  },
  {
    chain: CHAIN_ID,
    node: 'jp-iij',
    accept: { uuid: '6f1c2d3e-4b5a-4c6d-8e7f-90a1b2c3d4e5', label: 'jp-iij' },
    hop_in: { port: 22001, security: { t: 'none' } },
    rules: [
      { m: { t: 'geosite', v: ['openai', 'anthropic', 'google-gemini'] }, a: relay('us-gia') },
      { m: { t: 'any' }, a: { t: 'egress', send_through: null } },
    ],
  },
  {
    chain: CHAIN_ID,
    node: 'us-gia',
    accept: { uuid: '7a2b3c4d-5e6f-4a1b-9c2d-3e4f5a6b7c8d', label: 'us-gia' },
    hop_in: { port: 22002, security: { t: 'none' } },
    rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
  },
];

export const snapshot: ConsoleSnapshot = {
  snapshot: {
    revision: REVISION,
    nodes: [],
    external_outbounds: [],
    apps: [
      {
        id: APP_ID,
        label: '主线',
        chains: [{ id: CHAIN_ID, tenant: 'platform', name: CHAIN_NAME, subscription_country: 'JP' }],
        ingresses: [ingress],
        steps,
        fronts: [],
        grants: [],
      },
    ],
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;

export const settings = {
  ports: { ingress_base: 13443, anytls_base: 15443, hop_base: 22000, hy2_base: 32000 },
  relay_mux: DEFAULT_HOP_MUX,
  reality_site: {
    dest: 'site.example:443',
    server_names: ['site.example'],
    fingerprint: 'chrome',
    flow: 'xtls-rprx-vision',
  },
};

export const compiled = {
  revision: REVISION,
  summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
  diagnostics: [],
  system: {},
  apps: [],
  redacted: false,
};

/** 连通性横幅：入口每轮的端到端探测，最近 20 次都正常。 */
export function e2eProbes() {
  const now = nowSecs();
  const times = Array.from({ length: 20 }, (_, index) => now - (19 - index) * 600);
  return {
    chains: [
      {
        app_id: APP_ID,
        chain_id: CHAIN_ID,
        chain_name: CHAIN_NAME,
        node_id: 'hk-hkix',
        status: 'ok',
        ttfb_ms: 186,
        exit_ip: '198.51.100.51',
        exit_loc: 'JP',
        exit_verdict: 'match',
        detail: null,
        probed_at: new Date(now * 1000).toISOString().slice(0, 19).replace('T', ' '),
        samples: {
          probed_at_unix_secs: times,
          status: times.map(() => 'ok'),
          ttfb_ms: times.map((_, index) => 170 + ((index * 37) % 41)),
        },
      },
    ],
  };
}

/* ══ 检测结果 ══ */

type Spec = [UnlockService, UnlockResult['status'], UnlockResult['code'], string | null, boolean?];

const results = (specs: Spec[]): UnlockResult[] =>
  specs.map(([service, status, code, region, clientDns]) => ({
    service,
    status,
    code,
    region,
    ...(clientDns ? { client_dns: true } : {}),
  }));

/** YouTube 分项（Agent v49）：网页与 Premium 各自的结论、HTTP 状态与地区来源，加上视频 CDN 映射。 */
const page = (
  status: UnlockResult['status'],
  code: UnlockResult['code'],
  region: string | null,
  gl: string | null = region,
): YoutubePageResult => ({
  status,
  code,
  http_status: 200,
  page_region: gl,
  topbar_region: null,
  privacy_region: region,
});
const withYoutube = (list: UnlockResult[], youtube: YoutubeDiagnostics): UnlockResult[] =>
  list.map(result => (result.service === 'youtube_premium' ? { ...result, youtube } : result));

// 香港机器的默认出口：AI 三家都不支持香港，流媒体可用，TikTok 未识别、Reddit 被验证页拦下。
const HK_NOW_PLAIN = results([
  ['chatgpt', 'unsupported_region', 'region_unsupported', 'HK'],
  ['claude', 'unsupported_region', 'region_unsupported', 'HK'],
  ['gemini', 'unsupported_region', 'region_unsupported', 'HK'],
  ['netflix', 'available', 'web_available', 'HK'],
  ['disney_plus', 'available', 'web_available', 'HK'],
  ['youtube_premium', 'available', 'web_available', 'HK'],
  ['tiktok', 'failed', 'unrecognized_response', null],
  ['prime_video', 'available', 'web_available', 'HK'],
  ['reddit', 'restricted', 'access_denied', null],
]);
const HK_NOW = withYoutube(HK_NOW_PLAIN, {
  webpage: page('available', 'web_available', 'HK'),
  premium: page('available', 'web_available', 'HK'),
  cdn: { http_status: 200, node: 'hkg07s52', airport: 'HKG', country: 'HK', error: null, elapsed_ms: 230 },
});
// 前一次：Netflix 只放出自制剧，Prime Video 判成了新加坡——「与上次相比」要标出的两处。
const HK_BEFORE = HK_NOW.map(result =>
  result.service === 'netflix'
    ? { ...result, status: 'partial' as const, code: 'originals_only' as const }
    : result.service === 'prime_video'
      ? { ...result, region: 'SG' }
      : result,
);
const US_NOW_PLAIN = results([
  ['chatgpt', 'available', 'web_available', 'US'],
  ['claude', 'available', 'web_available', 'US'],
  ['gemini', 'available', 'web_available', 'US'],
  ['netflix', 'available', 'web_available', 'US'],
  ['disney_plus', 'available', 'web_available', 'US'],
  ['youtube_premium', 'available', 'web_available', 'US'],
  ['tiktok', 'available', 'web_available', 'US'],
  ['prime_video', 'available', 'web_available', 'US'],
  ['reddit', 'available', 'web_available', 'US'],
]);
const US_NOW = withYoutube(US_NOW_PLAIN, {
  webpage: page('available', 'web_available', 'US'),
  premium: page('available', 'web_available', 'US'),
  cdn: { http_status: 200, node: 'lax17s55', airport: 'LAX', country: 'US', error: null, elapsed_ms: 160 },
});
// 澳门：YouTube 网页能开，Premium 不支持澳门；页面配置 GL 是 US，不能当地区；视频从香港的节点出。
const MO_NOW = withYoutube(
  results([
    ['chatgpt', 'unsupported_region', 'region_unsupported', 'MO'],
    ['claude', 'unsupported_region', 'region_unsupported', 'MO'],
    ['gemini', 'unsupported_region', 'region_unsupported', 'MO'],
    ['netflix', 'available', 'web_available', 'MO'],
    ['disney_plus', 'unsupported_region', 'region_unsupported', 'MO'],
    ['youtube_premium', 'unsupported_region', 'region_unsupported', 'MO'],
    ['tiktok', 'unsupported_region', 'region_unsupported', 'MO'],
    ['prime_video', 'available', 'web_available', 'MO'],
    ['reddit', 'available', 'web_available', 'MO'],
  ]),
  {
    webpage: page('available', 'web_available', 'MO', 'US'),
    premium: page('unsupported_region', 'region_unsupported', 'MO', 'US'),
    cdn: { http_status: 200, node: 'hkg12s15', airport: 'HKG', country: 'HK', error: null, elapsed_ms: 1100 },
  },
);
// 链路：AI 经洛杉矶出网（US），其余在东京（JP）；YouTube 要靠入口先解析 IPv4 才通。
const CHAIN_NOW_PLAIN = results([
  ['chatgpt', 'available', 'web_available', 'US'],
  ['claude', 'available', 'web_available', 'US'],
  ['gemini', 'available', 'web_available', 'US'],
  ['netflix', 'partial', 'originals_only', 'JP'],
  ['disney_plus', 'available', 'web_available', 'JP'],
  ['youtube_premium', 'available', 'web_available', 'JP', true],
  ['tiktok', 'available', 'web_available', 'JP'],
  ['prime_video', 'available', 'web_available', 'JP'],
  ['reddit', 'restricted', 'access_denied', null],
]);
const CHAIN_NOW = withYoutube(CHAIN_NOW_PLAIN, {
  webpage: page('available', 'web_available', 'JP'),
  premium: page('available', 'web_available', 'JP'),
  cdn: { http_status: 200, node: 'nrt12s28', airport: 'NRT', country: 'JP', error: null, elapsed_ms: 880 },
});
const CHAIN_BEFORE = CHAIN_NOW.map(result =>
  result.service === 'chatgpt' ? { ...result, status: 'partial' as const, code: 'web_only' as const } : result,
);
const PATH_FAILED = CHAIN_NOW.map(result => ({
  service: result.service,
  status: 'failed' as const,
  code: 'network_error' as const,
  region: null,
}));

/* ══ 场景 ══ */

export type Scope = 'node' | 'chain';
export type SceneKey =
  'typical' | 'clear' | 'macau' | 'running' | 'queued' | 'never' | 'expired' | 'obsolete' | 'path' | 'old';

export interface Scene {
  label: string;
  scopes: Scope[];
  /** Machine shown on the 机器 page; the chain page always uses its ingress, 香港 HKIX. */
  machine: string;
  protocol: number;
  job: (scope: Scope) => NodeUnlockProbe | null;
  /** Results a manual run in this scene completes with. */
  fresh: (scope: Scope) => UnlockResult[];
}

const HOUR = 3600;

function job(scope: Scope, patch: Partial<NodeUnlockProbe>): NodeUnlockProbe {
  const now = nowSecs();
  const completed = now - 3 * HOUR;
  return {
    id: `mock-${scope}`,
    node_id: 'hk-hkix',
    state: 'complete',
    trigger: 'scheduled',
    created_at: completed - 74,
    completed_at: completed,
    next_run_at: completed + 24 * HOUR,
    results: scope === 'chain' ? CHAIN_NOW : HK_NOW,
    previous_results: scope === 'chain' ? CHAIN_BEFORE : HK_BEFORE,
    previous_completed_at: completed - 24 * HOUR,
    ...(scope === 'chain'
      ? { app_id: APP_ID, chain_id: CHAIN_ID, protocol: 'VLESS REALITY', configuration_revision: REVISION }
      : {}),
    path_error: null,
    ...patch,
  };
}

const freshOf = (scope: Scope) => (scope === 'chain' ? CHAIN_NOW : HK_NOW);
const typicalAt = (scope: Scope) => job(scope, {});

export const SCENES: Record<SceneKey, Scene> = {
  typical: {
    label: '典型 · 与上次有变化',
    scopes: ['node', 'chain'],
    machine: 'hk-hkix',
    protocol: 48,
    job: scope => typicalAt(scope),
    fresh: freshOf,
  },
  clear: {
    label: '全部可用',
    scopes: ['node'],
    machine: 'us-gia',
    protocol: 48,
    job: scope => job(scope, { node_id: 'us-gia', results: US_NOW, previous_results: US_NOW }),
    fresh: () => US_NOW,
  },
  macau: {
    label: 'YouTube 网页可用、Premium 不支持',
    scopes: ['node'],
    machine: 'mo-ctm',
    protocol: 49,
    job: scope => job(scope, { node_id: 'mo-ctm', results: MO_NOW, previous_results: MO_NOW }),
    fresh: () => MO_NOW,
  },
  running: {
    label: '检测中',
    scopes: ['node', 'chain'],
    machine: 'hk-hkix',
    protocol: 48,
    job: scope => {
      const now = nowSecs();
      const last = typicalAt(scope);
      return {
        ...last,
        state: 'running',
        trigger: 'manual',
        created_at: now - 42,
        completed_at: null,
        next_run_at: now - 42 + 24 * HOUR,
        results: [],
        previous_results: last.results,
        previous_completed_at: last.completed_at,
      };
    },
    fresh: freshOf,
  },
  queued: {
    label: '等待机器响应',
    scopes: ['node', 'chain'],
    machine: 'hk-hkix',
    protocol: 48,
    job: scope => {
      const now = nowSecs();
      const last = typicalAt(scope);
      return {
        ...last,
        state: 'queued',
        trigger: 'manual',
        created_at: now - 4,
        completed_at: null,
        next_run_at: now - 4 + 24 * HOUR,
        results: [],
        previous_results: last.results,
        previous_completed_at: last.completed_at,
      };
    },
    fresh: freshOf,
  },
  never: {
    label: '尚未检测',
    scopes: ['node', 'chain'],
    machine: 'hk-hkix',
    protocol: 48,
    job: () => null,
    fresh: freshOf,
  },
  expired: {
    label: '超时未完成',
    scopes: ['node', 'chain'],
    machine: 'hk-hkix',
    protocol: 48,
    job: scope => {
      const now = nowSecs();
      const last = typicalAt(scope);
      return {
        ...last,
        state: 'expired',
        trigger: 'scheduled',
        created_at: now - 9 * 60,
        completed_at: null,
        next_run_at: now - 9 * 60 + 24 * HOUR,
        results: [],
        previous_results: last.results,
        previous_completed_at: (last.completed_at ?? now) - 21 * HOUR,
      };
    },
    fresh: freshOf,
  },
  obsolete: {
    label: '线路已变更',
    scopes: ['chain'],
    machine: 'hk-hkix',
    protocol: 48,
    job: scope => job(scope, { state: 'obsolete', configuration_revision: REVISION - 1 }),
    fresh: freshOf,
  },
  path: {
    label: '检测客户端不可用',
    scopes: ['chain'],
    machine: 'hk-hkix',
    protocol: 48,
    job: scope =>
      job(scope, {
        path_error: 'client_unavailable',
        results: PATH_FAILED,
        completed_at: nowSecs() - 20 * 60,
        created_at: nowSecs() - 22 * 60,
        next_run_at: nowSecs() - 20 * 60 + 24 * HOUR,
        previous_completed_at: nowSecs() - 20 * 60 - 24 * HOUR,
      }),
    fresh: freshOf,
  },
  old: {
    label: 'Agent 需升级',
    scopes: ['node'],
    machine: 'hk-hkix',
    protocol: 45,
    job: () => null,
    fresh: freshOf,
  },
};

export const SCENE_KEYS = Object.keys(SCENES) as SceneKey[];
