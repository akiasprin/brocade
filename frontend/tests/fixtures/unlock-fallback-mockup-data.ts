// Synthetic chain unlock reports for the DNS 回退 mockup. Never imported by the app.
// The scene's chain gets its own IPv4 / IPv6 jobs; every other chain reuses the 线路页解锁 data, so the list page
// renders the same fleet. Jobs keep the shape the Console returns, plus one assumed field: `dns_paths`.
import type {
  DestMatch,
  EgressDnsResolution,
  NodeUnlockProbe,
  UnlockIpFamily,
  UnlockResult,
  UnlockService,
  YoutubePageResult,
} from '../../src/api';
import { APPS } from './list-head-mockup-data';
import { REVISION, nowSecs, unlockCapabilities, unlockJob as fleetJob } from './chain-unlock-mockup-data';

export { nowSecs };

/* ══ 假设的接口扩展 ══ */

/**
 * One service whose probe hostnames match a machine DNS policy on the chain's final exit. The Console would derive
 * it from the Serving revision the job ran against and freeze it with the job, like `configuration_revision`.
 * Services that match no policy are absent; they resolve through the machine DNS in both families.
 */
export interface UnlockDnsPath {
  service: UnlockService;
  /** The final exit machine that resolves this service. */
  exit: string;
  selector: DestMatch;
  resolution: EgressDnsResolution;
}
export type FallbackProbe = NodeUnlockProbe & { dns_paths?: UnlockDnsPath[] };

/**
 * How ordinary traffic treats one family of one service.
 * - normal: ordinary traffic can use this family, so its result applies.
 * - fallback: the policy resolver returns no address of this family; the probe reached it only because the lookup
 *   fell back to the machine DNS, which ordinary traffic does only when the policy resolver fails.
 * - excluded: the same, but the policy stops instead of falling back, so this family is never used.
 */
export type Route = 'normal' | 'fallback' | 'excluded';

export function routeOf(path: UnlockDnsPath | undefined, family: UnlockIpFamily): Route {
  if (!path) return 'normal';
  const strategy = path.resolution.address_strategy;
  const excludes = family === 'v6' ? strategy === 'use_ipv4' : strategy === 'use_ipv6';
  if (!excludes) return 'normal';
  return path.resolution.fallback === 'machine' ? 'fallback' : 'excluded';
}

/** The families ordinary traffic uses for a service; `null` = the machine DNS decides, so both apply. */
export function familiesOf(path: UnlockDnsPath | undefined): UnlockIpFamily[] {
  const strategy = path?.resolution.address_strategy;
  return strategy === 'use_ipv4' ? ['v4'] : strategy === 'use_ipv6' ? ['v6'] : ['v4', 'v6'];
}

/* ══ 检测结果 ══ */

type Outcome = readonly [UnlockResult['status'], UnlockResult['code']];
const OK: Outcome = ['available', 'web_available'];
const REGION: Outcome = ['unsupported_region', 'region_unsupported'];
const ORIGINALS: Outcome = ['partial', 'originals_only'];
const NO_AAAA: Outcome = ['skipped', 'no_ipv6_dns'];

/** Where a service lands, as each service reports it: Gemini answers in ISO alpha-3, Reddit with a Fastly POP. */
interface Place {
  region: string;
  iso3: string;
  pop: string;
  cdn: readonly [node: string, airport: string, country: string];
}
const HK: Place = { region: 'HK', iso3: 'HKG', pop: 'HKG', cdn: ['hkg07s52', 'HKG', 'HK'] };
const SG: Place = { region: 'SG', iso3: 'SGP', pop: 'SIN', cdn: ['sin11s04', 'SIN', 'SG'] };
const TW: Place = { region: 'TW', iso3: 'TWN', pop: 'HKG', cdn: ['tpe01s20', 'TPE', 'TW'] };
const JP: Place = { region: 'JP', iso3: 'JPN', pop: 'NRT', cdn: ['nrt12s28', 'NRT', 'JP'] };

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

/** Per-service outcome and, when the service lands elsewhere (an unlock DNS proxy), its place. */
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

/* ══ DNS 策略 ══ */

const AI: UnlockService[] = ['chatgpt', 'claude', 'gemini'];
const STREAMING: UnlockService[] = ['netflix', 'disney_plus'];

/** 文档地址段代替真实解锁 DNS；`same` 场景用公共解析器。 */
const AI_UNLOCK: EgressDnsResolution = {
  address: '198.51.100.53',
  port: 53,
  transport: 'tcp',
  address_strategy: 'use_ipv4',
  fallback: 'machine',
};
const MEDIA_UNLOCK: EgressDnsResolution = { ...AI_UNLOCK, address: '203.0.113.53' };
const PUBLIC_V4: EgressDnsResolution = { ...AI_UNLOCK, address: '1.1.1.1' };

const paths = (exit: string, selector: DestMatch, resolution: EgressDnsResolution, services: UnlockService[]) =>
  services.map<UnlockDnsPath>(service => ({ service, exit, selector, resolution }));
const AI_SELECTOR: DestMatch = { t: 'geosite', v: ['category-ai-!cn'] };
const MEDIA_SELECTOR: DestMatch = { t: 'geosite', v: ['disney', 'netflix'] };

/* ══ 场景 ══ */

export type SceneKey = 'unlock' | 'mixed' | 'same' | 'stop' | 'plain';
interface Scene {
  name: string;
  /** The chain whose detail page the mockup opens. */
  chain: string;
  v4: UnlockResult[];
  v6: UnlockResult[];
  dns: UnlockDnsPath[];
}

const AI_VIA_SG: Overrides = { chatgpt: [OK, SG], claude: [OK, SG], gemini: [OK, SG] };
const AI_CLOSED: Overrides = { chatgpt: REGION, claude: REGION, gemini: REGION };
const HK_NATIVE: Overrides = { tiktok: REGION };
// Netflix flags many hosting IPv6 ranges: a real IPv6 difference, unrelated to any DNS policy.
const HK_NATIVE_V6: Overrides = { ...HK_NATIVE, netflix: ORIGINALS };

export const SCENES: Record<SceneKey, Scene> = {
  unlock: {
    name: 'AI 走解锁 DNS · 回退后地区不支持',
    chain: 'hk-direct',
    v4: report(HK, { ...HK_NATIVE, ...AI_VIA_SG }),
    v6: report(HK, { ...HK_NATIVE_V6, ...AI_CLOSED }),
    dns: paths('hk-hkix', AI_SELECTOR, AI_UNLOCK, AI),
  },
  mixed: {
    name: 'AI 与 Netflix、Disney+ 各走一组解锁 DNS',
    chain: 'hk-direct',
    v4: report(HK, { ...HK_NATIVE, ...AI_VIA_SG, netflix: [OK, TW], disney_plus: [OK, TW] }),
    v6: report(HK, { ...HK_NATIVE_V6, ...AI_CLOSED }),
    dns: [
      ...paths('hk-hkix', AI_SELECTOR, AI_UNLOCK, AI),
      ...paths('hk-hkix', MEDIA_SELECTOR, MEDIA_UNLOCK, STREAMING),
    ],
  },
  same: {
    name: 'AI 走自定义 DNS · 两族结论相同',
    chain: 'jp-iij',
    v4: report(JP, { hbo_max: REGION }),
    v6: report(JP, { hbo_max: REGION, netflix: ORIGINALS }),
    dns: paths('jp-iij', AI_SELECTOR, PUBLIC_V4, AI),
  },
  stop: {
    name: '自定义 DNS 失败时停止连接',
    chain: 'hk-direct',
    v4: report(HK, { ...HK_NATIVE, ...AI_VIA_SG }),
    v6: report(HK, { ...HK_NATIVE_V6, chatgpt: NO_AAAA, claude: NO_AAAA, gemini: NO_AAAA }),
    dns: paths('hk-hkix', AI_SELECTOR, { ...AI_UNLOCK, fallback: 'stop' }, AI),
  },
  plain: {
    name: '出口没有 DNS 策略',
    chain: 'hk-direct',
    v4: report(HK, { ...HK_NATIVE, ...AI_CLOSED }),
    v6: report(HK, { ...HK_NATIVE_V6, ...AI_CLOSED }),
    dns: [],
  },
};

/* ══ 任务 ══ */

const HOUR = 3600;
const DAY = 24 * HOUR;
const appOf = (chain: string) => APPS.find(app => app.chains.some(item => item.id === chain))!;
const entryOf = (chain: string) => appOf(chain).ingresses.find(ingress => ingress.chain === chain)?.node ?? '';

export function unlockJob(chain: string, family: UnlockIpFamily, scene: SceneKey, now: number): FallbackProbe | null {
  const spec = SCENES[scene];
  if (chain !== spec.chain) {
    // 其余链两族结论相同，列表上的对比只落在场景链上。
    const own = fleetJob(chain, family, 'calm', now);
    const v4 = family === 'v6' && !own ? fleetJob(chain, 'v4', 'calm', now) : null;
    const job = own ?? (v4 && { ...v4, id: `${v4.id}-v6`, family });
    return job && { ...job, dns_paths: [] };
  }
  const completed = now - (family === 'v4' ? 3 : 2.6) * HOUR;
  const results = family === 'v4' ? spec.v4 : spec.v6;
  return {
    id: `mock-${chain}-${family}`,
    node_id: entryOf(chain),
    app_id: appOf(chain).id,
    chain_id: chain,
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
    dns_paths: spec.dns,
  };
}

export function capabilitiesOf(chain: string, scene: SceneKey) {
  return chain === SCENES[scene].chain
    ? { family_mode: 'split' as const, automatic_ready: false, ipv6_available: true, ipv6_reason: null }
    : unlockCapabilities(chain);
}
