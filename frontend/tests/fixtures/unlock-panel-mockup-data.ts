// Synthetic unlock reports for the 解锁面板整体设计 mockup. Never imported by the app.
// Jobs keep the production wire shape, including the measured DNS evidence (`dns_parts`, `connection_observations`)
// the landed A design reads, so 现状 renders exactly what production renders. Every other chain reuses the 线路页
// fleet so the list page stays populated.
import type {
  DestMatch,
  EgressDnsResolution,
  NodeUnlockProbe,
  UnlockConnectionObservation,
  UnlockDnsPartPath,
  UnlockDnsPath,
  UnlockIpFamily,
  UnlockPart,
  UnlockResult,
  UnlockService,
  YoutubePageResult,
} from '../../src/api';
import { APPS } from './list-head-mockup-data';
import { REVISION, nowSecs, unlockCapabilities, unlockJob as fleetJob } from './chain-unlock-mockup-data';

export { nowSecs };

/* ══ 检测结果 ══ */

type Outcome = readonly [UnlockResult['status'], UnlockResult['code']];
const OK: Outcome = ['available', 'web_available'];
const REGION: Outcome = ['unsupported_region', 'region_unsupported'];
const ORIGINALS: Outcome = ['partial', 'originals_only'];
const NO_AAAA: Outcome = ['skipped', 'no_ipv6_dns'];

interface Place {
  region: string;
  iso3: string;
  pop: string;
  cdn: readonly [node: string, airport: string, country: string];
}
const HK: Place = { region: 'HK', iso3: 'HKG', pop: 'HKG', cdn: ['hkg07s52', 'HKG', 'HK'] };
const SG: Place = { region: 'SG', iso3: 'SGP', pop: 'SIN', cdn: ['sin11s04', 'SIN', 'SG'] };

const CHECKED: UnlockService[] = [
  'chatgpt',
  'claude',
  'gemini',
  'netflix',
  'disney_plus',
  'youtube_premium',
  'tiktok',
  'prime_video',
  'reddit',
  'spotify',
  'hbo_max',
  'dazn',
];

type Overrides = Partial<Record<UnlockService | 'youtube', Outcome | readonly [Outcome, Place]>>;

function report(home: Place, overrides: Overrides = {}): UnlockResult[] {
  const read = (service: UnlockService | 'youtube'): [Outcome, Place] => {
    const value = overrides[service];
    if (!value) return [OK, home];
    return value.length === 2 && Array.isArray(value[0])
      ? [value[0] as Outcome, value[1] as Place]
      : [value as Outcome, home];
  };
  const regionOf = (service: UnlockService | 'youtube', [status]: Outcome, place: Place) =>
    status === 'failed' || status === 'restricted' || status === 'skipped'
      ? null
      : service === 'gemini'
        ? place.iso3
        : service === 'reddit'
          ? place.pop
          : place.region;
  const page = ([status, code]: Outcome, region: string | null): YoutubePageResult => ({
    status,
    code,
    http_status: status === 'failed' || status === 'skipped' ? null : 200,
    page_region: region,
    topbar_region: null,
    privacy_region: region,
  });
  return CHECKED.map(service => {
    const [outcome, place] = read(service);
    const result: UnlockResult = {
      service,
      status: outcome[0],
      code: outcome[1],
      region: regionOf(service, outcome, place),
    };
    if (service !== 'youtube_premium') return result;
    const [webpage] = read('youtube');
    const [node, airport, country] = place.cdn;
    return {
      ...result,
      youtube: {
        webpage: page(webpage, regionOf('youtube', webpage, place)),
        premium: page(outcome, result.region),
        cdn: { http_status: 200, node, airport, country, error: null, elapsed_ms: 180 },
      },
    };
  });
}

/* ══ 实测解析路径（生产 A 读取的同一字段） ══ */

const EXIT = 'hk-hkix';
const AI_SELECTOR: DestMatch = { t: 'geosite', v: ['category-ai-!cn'] };
const GOOGLE_SELECTOR: DestMatch = { t: 'geosite', v: ['google'] };
/** 文档地址段代替真实解锁 DNS。 */
const AI_UNLOCK: EgressDnsResolution = {
  address: '198.51.100.53',
  port: 53,
  transport: 'tcp',
  address_strategy: 'use_ipv4',
  fallback: 'machine',
};
const GOOGLE_V6: EgressDnsResolution = {
  address: '2001:db8:53::1',
  port: 53,
  transport: 'udp',
  address_strategy: 'use_ipv6',
  fallback: 'machine',
};

function path(
  selector: DestMatch,
  resolution: EgressDnsResolution,
  normal: UnlockIpFamily,
  fallback: 'machine' | 'stop',
): UnlockDnsPath {
  return {
    exit: EXIT,
    normal_family: normal,
    fallback,
    rules: [{ selector, resolution: { ...resolution, fallback } }],
    ...(fallback === 'machine' ? { answer_from: 'machine' as const } : {}),
    reason: 'no_address',
  };
}

function connection(
  part: UnlockPart,
  serial: number,
  domain: string,
  family: UnlockIpFamily,
  fellBack: boolean,
  connected = true,
): UnlockConnectionObservation {
  return {
    part,
    version: 2,
    id: serial.toString(16).padStart(32, '0'),
    domain,
    exit: EXIT,
    outbound: `out:egress:probe-${family}`,
    profile: 'a'.repeat(64),
    normal_strategy: 'UseIP',
    family,
    connected,
    complete: true,
    ...(connected ? {} : { error: 'dns_empty' }),
    lookups: [
      {
        version: 1,
        domain,
        strategy: family === 'v4' ? 'FORCE_IP4' : 'FORCE_IP6',
        complete: true,
        steps: fellBack
          ? [
              { tag: 'dns:egress:23aad4af493f', scoped: true, outcome: 'empty', other: true },
              { tag: 'xray.system.synthetic', scoped: false, outcome: 'answer', other: false },
            ]
          : [{ tag: 'xray.system.synthetic', scoped: false, outcome: 'answer', other: false }],
      },
    ],
  };
}

const AI_DOMAINS: Partial<Record<UnlockService, string>> = {
  chatgpt: 'chatgpt.com',
  claude: 'claude.ai',
  gemini: 'gemini.google.com',
};

/** IPv6 AI requests: the IPv4-only rule DNS answers nothing, so the lookup continues to the machine DNS. */
function aiFallback(results: UnlockResult[], fallback: 'machine' | 'stop'): UnlockResult[] {
  return results.map((result, index) => {
    const domain = AI_DOMAINS[result.service];
    if (!domain) return result;
    const dns = path(AI_SELECTOR, AI_UNLOCK, 'v4', fallback);
    const part: UnlockDnsPartPath = { part: 'service', kind: 'fallback', paths: [dns], exits: [EXIT] };
    return {
      ...result,
      ...(fallback === 'stop' ? { status: NO_AAAA[0], code: NO_AAAA[1], region: null } : {}),
      dns_path: dns,
      dns_parts: [part],
      connection_requests: { service: 1 },
      connection_observations: [connection('service', 100 + index, domain, 'v6', true, fallback === 'machine')],
    };
  });
}

/** IPv4 YouTube requests: the Google rule DNS returns IPv6 only; the webpage fell back, Premium partly did. */
function youtubeFallback(results: UnlockResult[]): UnlockResult[] {
  return results.map(result => {
    if (result.service !== 'youtube_premium') return result;
    const dns = path(GOOGLE_SELECTOR, GOOGLE_V6, 'v6', 'machine');
    return {
      ...result,
      dns_parts: [
        { part: 'webpage', kind: 'fallback', paths: [dns], exits: [EXIT] },
        { part: 'premium', kind: 'partial', paths: [dns], exits: [EXIT] },
        { part: 'cdn', kind: 'direct', paths: [], exits: [EXIT] },
      ],
      connection_requests: { webpage: 1, premium: 2, cdn: 1 },
      connection_observations: [
        connection('webpage', 201, 'www.youtube.com', 'v4', true),
        connection('premium', 202, 'www.youtube.com', 'v4', true),
        connection('premium', 203, 'm.youtube.com', 'v4', false),
        connection('cdn', 204, 'redirector.googlevideo.com', 'v4', false),
      ],
    };
  });
}

/** Exercise the evidence fields that a compact path view must preserve. */
function variedPaths(results: UnlockResult[]): UnlockResult[] {
  const laterRule = {
    selector: AI_SELECTOR,
    resolution: { ...AI_UNLOCK, address: '198.51.100.54', address_strategy: 'use_ip' as const },
  };
  return results.map(result => {
    const original = result.dns_parts?.[0];
    const first = original?.paths[0];
    if (!original || !first || result.service === 'gemini') return result;
    const later: UnlockDnsPath = {
      ...first,
      answer_from: 'policy',
      reason: 'resolver_error',
      rules: [...first.rules, laterRule],
    };
    const records = (result.connection_observations ?? []).map(record => ({
      ...record,
      lookups: record.lookups.map(lookup => ({
        ...lookup,
        steps: [
          { tag: 'dns:egress:23aad4af493f', scoped: true, outcome: 'error' as const, other: true },
          { tag: 'dns:egress:later', scoped: true, outcome: 'answer' as const, other: true },
        ],
      })),
    }));
    const multiple = result.service === 'claude';
    return {
      ...result,
      dns_path: multiple ? null : later,
      dns_parts: [{ ...original, paths: multiple ? [first, later] : [later] }],
      connection_requests: { service: multiple ? 2 : 1 },
      connection_observations: multiple
        ? [...(result.connection_observations ?? []), ...records.map(record => ({ ...record, id: 'f'.repeat(32) }))]
        : records,
    };
  });
}

/* ══ 场景 ══ */

export type SceneKey = 'ai' | 'youtube' | 'stop' | 'paths';
interface Scene {
  name: string;
  v4: UnlockResult[];
  v6: UnlockResult[];
}

// 香港出口：AI 经解锁 DNS（只返回 IPv4）落到新加坡；IPv6 回退到机器 DNS 后由香港原生访问。
const AI_VIA_SG: Overrides = { chatgpt: [OK, SG], claude: [OK, SG], gemini: [OK, SG] };
const AI_CLOSED: Overrides = { chatgpt: REGION, claude: REGION, gemini: REGION };
const V4 = report(HK, { tiktok: REGION, ...AI_VIA_SG });
// Netflix flags many hosting IPv6 ranges: a real IPv6 difference, unrelated to any DNS rule.
const V6 = report(HK, { tiktok: REGION, netflix: ORIGINALS, ...AI_CLOSED });

export const SCENES: Record<SceneKey, Scene> = {
  ai: { name: 'AI 走 IPv4 解锁 DNS · IPv6 回退', v4: V4, v6: aiFallback(V6, 'machine') },
  youtube: {
    name: '另有 YouTube 只解析 IPv6 · IPv4 回退',
    v4: youtubeFallback(V4),
    v6: aiFallback(V6, 'machine'),
  },
  stop: { name: '失败处理为停止连接', v4: V4, v6: aiFallback(V6, 'stop') },
  paths: {
    name: '多条路径 · 后续自定义 / 机器 DNS',
    v4: V4,
    v6: variedPaths(aiFallback(V6, 'machine')),
  },
};

/** The machine's own probes resolve through the system resolver: no rule DNS, so AI is HK-native in both families. */
const MACHINE = { v4: report(HK, { tiktok: REGION, ...AI_CLOSED }), v6: V6 };

/* ══ 任务 ══ */

export const SCENE_CHAIN = 'hk-direct';
export const SCENE_NODE = 'hk-hkix';
const HOUR = 3600;
const DAY = 24 * HOUR;
const appOf = (chain: string) => APPS.find(app => app.chains.some(item => item.id === chain))!;

function job(results: UnlockResult[], family: UnlockIpFamily, now: number, owner: object): NodeUnlockProbe {
  const completed = now - (family === 'v4' ? 3 : 2.6) * HOUR;
  return {
    id: `mock-${family}-${completed}`,
    node_id: SCENE_NODE,
    family,
    state: 'complete',
    trigger: 'scheduled',
    created_at: completed - 81,
    completed_at: completed,
    next_run_at: completed + DAY,
    results,
    previous_results: results,
    previous_completed_at: completed - DAY,
    protocol: 'VLESS REALITY',
    configuration_revision: REVISION,
    path_error: null,
    ...owner,
  };
}

export function chainJob(chain: string, family: UnlockIpFamily, scene: SceneKey, now: number) {
  if (chain !== SCENE_CHAIN) {
    // 其余链两族结论相同，列表上的对比只落在场景链上。
    const own = fleetJob(chain, family, 'calm', now);
    const v4 = family === 'v6' && !own ? fleetJob(chain, 'v4', 'calm', now) : null;
    return own ?? (v4 && { ...v4, id: `${v4.id}-v6`, family });
  }
  return job(SCENES[scene][family], family, now, { app_id: appOf(chain).id, chain_id: chain });
}

export const nodeJob = (family: UnlockIpFamily, now: number) => job(MACHINE[family], family, now, {});

export const chainCapabilities = (chain: string) =>
  chain === SCENE_CHAIN
    ? { family_mode: 'split' as const, automatic_ready: false, ipv6_available: true, ipv6_reason: null }
    : unlockCapabilities(chain);
