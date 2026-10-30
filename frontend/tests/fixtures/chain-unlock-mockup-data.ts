// Synthetic chain unlock reports for the 线路页解锁 mockup. Never imported by the app.
// Chains, machines and end-to-end probes come from the list page mockup, so the production list renders the same
// fleet. Every job keeps the shape the Console returns (UnlockProbeView): previous results stay after a run, a path
// failure reports nine failures, and a running job has no results of its own.
import type {
  NodeUnlockProbe,
  UnlockIpFamily,
  UnlockProbeCapabilities,
  UnlockResult,
  UnlockService,
  YoutubePageResult,
} from '../../src/api';
import { APPS } from './list-head-mockup-data';

export const nowSecs = () => Math.floor(Date.now() / 1000);
const HOUR = 3600;
const DAY = 24 * HOUR;
export const REVISION = 41;

/* ══ 检测结果 ══ */

type Outcome = readonly [UnlockResult['status'], UnlockResult['code']];
const OK: Outcome = ['available', 'web_available'];
const REGION: Outcome = ['unsupported_region', 'region_unsupported'];
const DENIED: Outcome = ['restricted', 'access_denied'];
const ORIGINALS: Outcome = ['partial', 'originals_only'];
const WEB_ONLY: Outcome = ['partial', 'web_only'];
const UNRECOGNIZED: Outcome = ['failed', 'unrecognized_response'];

/** Where a chain's services land, as each service reports it: Gemini answers in ISO alpha-3, Reddit with the
 *  Fastly POP that served it, YouTube's video CDN with a Google node name. */
interface Place {
  region: string;
  iso3: string;
  pop: string;
  cdn: readonly [node: string, airport: string, country: string];
}

const HK: Place = { region: 'HK', iso3: 'HKG', pop: 'HKG', cdn: ['hkg07s52', 'HKG', 'HK'] };
const JP: Place = { region: 'JP', iso3: 'JPN', pop: 'NRT', cdn: ['nrt12s28', 'NRT', 'JP'] };
const TW: Place = { region: 'TW', iso3: 'TWN', pop: 'HKG', cdn: ['tpe01s20', 'TPE', 'TW'] };
const SG: Place = { region: 'SG', iso3: 'SGP', pop: 'SIN', cdn: ['sin11s04', 'SIN', 'SG'] };
const KR: Place = { region: 'KR', iso3: 'KOR', pop: 'ICN', cdn: ['icn12s01', 'ICN', 'KR'] };
const LA: Place = { region: 'US', iso3: 'USA', pop: 'LAX', cdn: ['lax17s55', 'LAX', 'US'] };
const SJ: Place = { region: 'US', iso3: 'USA', pop: 'SJC', cdn: ['sjc07s30', 'SJC', 'US'] };
const MO: Place = { region: 'MO', iso3: 'MAC', pop: 'HKG', cdn: ['hkg12s15', 'HKG', 'HK'] };
const DE: Place = { region: 'DE', iso3: 'DEU', pop: 'FRA', cdn: ['fra16s52', 'FRA', 'DE'] };
const GB: Place = { region: 'GB', iso3: 'GBR', pop: 'LHR', cdn: ['lhr25s34', 'LHR', 'GB'] };

/** Agent v54 checks twelve services; YouTube Premium carries the ordinary YouTube page as `youtube.webpage`. */
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

type Overrides = Partial<Record<UnlockService | 'youtube', Outcome>>;

function report(place: Place, overrides: Overrides = {}): UnlockResult[] {
  const regionOf = (service: UnlockService | 'youtube', [status]: Outcome) =>
    status === 'failed' || status === 'restricted'
      ? null
      : service === 'gemini'
        ? place.iso3
        : service === 'reddit'
          ? place.pop
          : place.region;
  const page = ([status, code]: Outcome, region: string | null): YoutubePageResult => ({
    status,
    code,
    http_status: status === 'failed' ? null : 200,
    page_region: region,
    topbar_region: null,
    privacy_region: region,
  });
  return CHECKED.map(service => {
    const outcome = overrides[service] ?? OK;
    const result: UnlockResult = { service, status: outcome[0], code: outcome[1], region: regionOf(service, outcome) };
    if (service !== 'youtube_premium') return result;
    const webpage = overrides.youtube ?? OK;
    const [node, airport, country] = place.cdn;
    return {
      ...result,
      youtube: {
        webpage: page(webpage, regionOf('youtube', webpage)),
        premium: page(outcome, result.region),
        cdn: { http_status: 200, node, airport, country, error: null, elapsed_ms: 190 },
      },
    };
  });
}

const pathFailed = (): UnlockResult[] =>
  CHECKED.map(service => ({ service, status: 'failed', code: 'network_error', region: null }));

/* ══ 每条链的任务 ══ */

const appOf = (chain: string) => APPS.find(app => app.chains.some(item => item.id === chain))!;
const entryOf = (chain: string) => appOf(chain).ingresses.find(ingress => ingress.chain === chain)?.node ?? null;

interface Run {
  /** Seconds since the shown run completed. */
  age: number;
  results: UnlockResult[];
  /** The run before it; omitted = the same results, so nothing reads as changed. */
  before?: UnlockResult[];
  state?: NodeUnlockProbe['state'];
  path_error?: NodeUnlockProbe['path_error'];
  configuration_revision?: number;
}

function job(chain: string, family: UnlockIpFamily, run: Run, now: number): NodeUnlockProbe {
  const completed = now - run.age;
  return {
    id: `mock-${chain}-${family}`,
    node_id: entryOf(chain) ?? '',
    app_id: appOf(chain).id,
    chain_id: chain,
    family,
    state: run.state ?? 'complete',
    trigger: 'scheduled',
    created_at: completed - 74,
    completed_at: completed,
    next_run_at: completed + DAY,
    results: run.results,
    previous_results: run.before ?? run.results,
    previous_completed_at: completed - DAY,
    protocol: 'VLESS REALITY',
    configuration_revision: run.configuration_revision ?? REVISION,
    path_error: run.path_error ?? null,
  };
}

/** A run in progress or one that timed out: no results of its own, the previous run is what the page shows. */
function waiting(
  chain: string,
  family: UnlockIpFamily,
  state: 'running' | 'expired',
  previous: UnlockResult[],
  now: number,
) {
  const started = state === 'running' ? now - 42 : now - 25 * 60;
  return {
    ...job(chain, family, { age: 0, results: [] }, now),
    state,
    trigger: state === 'running' ? ('manual' as const) : ('scheduled' as const),
    created_at: started,
    completed_at: null,
    next_run_at: started + DAY,
    results: [],
    previous_results: previous,
    previous_completed_at: now - DAY - 2 * HOUR,
  };
}

export type SceneKey = 'typical' | 'calm';
export const SCENES: Record<SceneKey, string> = {
  typical: '典型 · 含检测中、超时、线路变更',
  calm: '全部已检测',
};

type Jobs = (now: number) => NodeUnlockProbe | null;
interface ChainReports {
  v4: Record<SceneKey, Jobs>;
  /** Missing = the chain's final exit has no IPv6 address. */
  v6?: Jobs;
}

const AI_CLOSED: Overrides = { chatgpt: REGION, claude: REGION, gemini: REGION };
const HK_TYPICAL = report(HK, { ...AI_CLOSED, netflix: ORIGINALS, tiktok: REGION, reddit: DENIED });
const HK_BEFORE = report(HK, { ...AI_CLOSED, tiktok: REGION, reddit: DENIED });
const JP_ALL = report(JP, { hbo_max: REGION });
const TW_ALL = report(TW);
const SG_ALL = report(SG);
const KR_ALL = report(KR, { hbo_max: REGION });
const US_GIA = report(LA);
const US_GIA_BEFORE = report(LA, { netflix: ORIGINALS });
const US_DC = report(LA, { netflix: ORIGINALS, disney_plus: DENIED, reddit: DENIED });
const MO_ALL = report(MO, {
  ...AI_CLOSED,
  disney_plus: REGION,
  youtube_premium: REGION,
  tiktok: REGION,
  hbo_max: REGION,
});
const SJC_ALL = report(SJ, { chatgpt: WEB_ONLY, netflix: ORIGINALS, tiktok: UNRECOGNIZED });
const DE_ALL = report(DE, { hbo_max: REGION });
const GB_ALL = report(GB, { hbo_max: REGION });

const same = (chain: string, run: Run): Record<SceneKey, Jobs> => ({
  typical: now => job(chain, 'v4', run, now),
  calm: now => job(chain, 'v4', run, now),
});

// prettier-ignore
const REPORTS: Record<string, ChainReports> = {
  'hk-direct': {
    v4: same('hk-direct', { age: 3 * HOUR, results: HK_TYPICAL, before: HK_BEFORE }),
    v6: now => job('hk-direct', 'v6', { age: 3 * HOUR, results: HK_BEFORE }, now),
  },
  'jp-iij': {
    v4: same('jp-iij', { age: 5 * HOUR, results: JP_ALL }),
    v6: now => job('jp-iij', 'v6', { age: 5 * HOUR, results: report(JP, { hbo_max: REGION, chatgpt: DENIED }) }, now),
  },
  'jp-sb': {
    v4: {
      typical: now => waiting('jp-sb', 'v4', 'expired', JP_ALL, now),
      calm: now => job('jp-sb', 'v4', { age: 10 * HOUR, results: JP_ALL }, now),
    },
  },
  'tw-hinet': {
    v4: {
      typical: now => waiting('tw-hinet', 'v4', 'running', TW_ALL, now),
      calm: now => job('tw-hinet', 'v4', { age: 1 * HOUR, results: TW_ALL }, now),
    },
  },
  'sg-do': {
    v4: {
      typical: now => job('sg-do', 'v4', { age: 8 * HOUR, results: SG_ALL, state: 'obsolete', configuration_revision: REVISION - 1 }, now),
      calm: now => job('sg-do', 'v4', { age: 8 * HOUR, results: SG_ALL }, now),
    },
  },
  'kr-kt': { v4: same('kr-kt', { age: 11 * HOUR, results: KR_ALL }) },
  'us-gia': { v4: same('us-gia', { age: 2 * HOUR, results: US_GIA, before: US_GIA_BEFORE }) },
  'us-9929': {
    v4: {
      typical: now => ({
        ...job('us-9929', 'v4', { age: 20 * 60, results: pathFailed(), path_error: 'client_unavailable' }, now),
        previous_results: report(LA, { disney_plus: DENIED }),
      }),
      calm: now => job('us-9929', 'v4', { age: 13 * HOUR, results: report(LA, { disney_plus: DENIED }) }, now),
    },
  },
  'us-cmin2': { v4: same('us-cmin2', { age: 7 * HOUR, results: US_DC }) },
  'mo-ctm': { v4: same('mo-ctm', { age: 4 * HOUR, results: MO_ALL }) },
  'us-sjc': { v4: same('us-sjc', { age: 14 * HOUR, results: SJC_ALL }) },
  'us-sea': {
    v4: {
      typical: () => null,
      calm: now => job('us-sea', 'v4', { age: 16 * HOUR, results: report(SJ, { netflix: ORIGINALS }) }, now),
    },
  },
  'de-fra': { v4: same('de-fra', { age: 9 * HOUR, results: DE_ALL }) },
  'gb-lon': {
    v4: same('gb-lon', { age: 6 * HOUR, results: GB_ALL }),
    v6: now => job('gb-lon', 'v6', { age: 6 * HOUR, results: GB_ALL }, now),
  },
  'kr-old': { v4: same('kr-old', { age: 6 * DAY, results: KR_ALL }) },
};

export function unlockJob(chain: string, family: UnlockIpFamily, scene: SceneKey, now: number): NodeUnlockProbe | null {
  const reports = REPORTS[chain];
  if (!reports) return null;
  return family === 'v6' ? (reports.v6?.(now) ?? null) : reports.v4[scene](now);
}

export function unlockCapabilities(chain: string): UnlockProbeCapabilities {
  const ipv6 = REPORTS[chain]?.v6 !== undefined;
  return {
    family_mode: 'split',
    automatic_ready: false,
    ipv6_available: ipv6,
    ipv6_reason: ipv6 ? null : '链路出口无 IPv6 地址',
  };
}
