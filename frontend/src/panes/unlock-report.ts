import type {
  NodeUnlockProbe,
  UnlockResult,
  UnlockService,
  YoutubeDiagnostics,
  UnlockDnsPath,
  UnlockIpFamily,
  UnlockDnsPartPath,
} from '../api';
import { verdictOf, type Verdict } from './unlock-status';
import { youtubeResult } from './youtube-unlock';

type Category = 'ai' | 'media' | 'social';
export const UNLOCK_SERVICE_NAMES = {
  chatgpt: 'ChatGPT',
  claude: 'Claude',
  gemini: 'Gemini',
  netflix: 'Netflix',
  disney_plus: 'Disney+',
  youtube_premium: 'YouTube Premium',
  tiktok: 'TikTok',
  prime_video: 'Prime Video',
  reddit: 'Reddit',
  spotify: 'Spotify',
  hbo_max: 'HBO Max',
  dazn: 'DAZN',
  // Keep historical wire identities readable; retired services are excluded below.
  bbc_iplayer: 'BBC iPlayer',
  crunchyroll: 'Crunchyroll',
  paramount_plus: 'Paramount+',
} as const;
export type DisplayService = UnlockService | 'youtube';
export const DISPLAY_SERVICE_NAMES = { ...UNLOCK_SERVICE_NAMES, youtube: 'YouTube' };
export const SERVICES = (Object.keys(UNLOCK_SERVICE_NAMES) as UnlockService[])
  .filter(service => !['bbc_iplayer', 'crunchyroll', 'paramount_plus'].includes(service))
  .flatMap<DisplayService>(service => (service === 'youtube_premium' ? ['youtube', service] : [service]));
export const CATEGORY: Record<DisplayService, Category> = {
  chatgpt: 'ai',
  claude: 'ai',
  gemini: 'ai',
  netflix: 'media',
  disney_plus: 'media',
  youtube: 'media',
  youtube_premium: 'media',
  prime_video: 'media',
  tiktok: 'social',
  reddit: 'social',
  spotify: 'media',
  hbo_max: 'media',
  dazn: 'media',
  bbc_iplayer: 'media',
  crunchyroll: 'media',
  paramount_plus: 'media',
};
export const CATEGORIES: { key: Category; name: string }[] = [
  { key: 'ai', name: 'AI' },
  { key: 'media', name: '流媒体' },
  { key: 'social', name: '社区' },
];

export interface Row {
  dnsPath: UnlockDnsPath | null;
  dnsPart?: UnlockDnsPartPath | null;
  family: UnlockIpFamily | null;
  route: 'normal' | 'fallback' | 'excluded';
  service: DisplayService;
  name: string;
  category: Category;
  result: UnlockResult | null;
  /** Raw service result before projecting YouTube webpage and Premium. */
  source: UnlockResult | null;
  verdict: Verdict;
  region: string | null;
  /** 与上一次结果不同时的上次读数，例如「上次 仅自制剧」「上次 SG」。 */
  change: string | null;
  /** YouTube 分项（Agent v49 起）；旧结果没有，这时不显示分项入口。 */
  youtube: YoutubeDiagnostics | null;
}

const WAITING: Verdict = { tone: 'wait', label: '等待结果', note: null, reason: '检测完成后显示' };
const MISSING: Verdict = { tone: 'unknown', label: '未取得', note: null, reason: '本次没有这项结果' };

function changeOf(now: UnlockResult, before: UnlockResult | undefined, service: DisplayService): string | null {
  if (!before || before.status === 'failed') return null;
  const a = verdictOf(now, service);
  const b = verdictOf(before, service);
  const parts: string[] = [];
  if (a.label !== b.label || a.note !== b.note) parts.push(b.note ? `${b.label}（${b.note}）` : b.label);
  if ((now.region ?? null) !== (before.region ?? null)) parts.push(before.region ?? '地区未知');
  return parts.length > 0 ? `上次 ${parts.join(' · ')}` : null;
}

export function unlockReport(job: NodeUnlockProbe | null | undefined) {
  const running = job?.state === 'queued' || job?.state === 'running';
  // 链路级失败（客户端没起来、检测期间线路变了）时所有项目都是「失败」，不是各服务的事实：
  // 与超时一样，改为显示上一次的结果并减淡。
  const current = job && job.results.length > 0 && !job.path_error ? job.results : null;
  const previous = !current && job?.previous_results?.length ? job.previous_results : null;
  const shown = current ?? previous;
  const shownAt = current ? job?.completed_at : previous ? job?.previous_completed_at : null;
  // 只给新鲜的结果标「与上次不同」：线路已变更时整块都是旧的，再逐项比较只是噪音。
  const before =
    current && job?.state === 'complete' && job?.previous_results?.length
      ? new Map(job.previous_results.map(r => [r.service, r]))
      : null;
  const rows: Row[] = SERVICES.map(service => {
    const sourceService = service === 'youtube' ? 'youtube_premium' : service;
    const source = shown?.find(item => item.service === sourceService);
    const project = (raw: UnlockResult | undefined) =>
      service === 'youtube'
        ? youtubeResult(raw, 'webpage')
        : service === 'youtube_premium'
          ? youtubeResult(raw, 'premium')
          : (raw ?? null);
    const result = project(source);
    const previousResult = project(before?.get(sourceService));
    const part = service === 'youtube' ? 'webpage' : service === 'youtube_premium' ? 'premium' : 'service';
    const dnsPart = source?.dns_parts?.find(evidence => evidence.part === part) ?? null;
    const dnsPath = dnsPart?.paths[0] ?? null;
    const route =
      dnsPart?.kind === 'fallback' &&
      job?.family &&
      dnsPart.paths.length > 0 &&
      dnsPart.paths.every(path => path.normal_family !== null && path.normal_family !== job.family)
        ? dnsPart.paths.every(path => path.fallback === 'stop')
          ? 'excluded'
          : 'fallback'
        : 'normal';
    const verdict = result ? verdictOf(result, service) : running ? WAITING : MISSING;
    if (route === 'excluded' && result?.status === 'skipped' && ['no_ipv6_dns', 'no_ipv4_dns'].includes(result.code)) {
      const normal = job?.family === 'v4' ? 'IPv6' : 'IPv4';
      const fixed = job?.family === 'v4' ? 'use_ipv6' : 'use_ipv4';
      verdict.reason = dnsPath?.rules.every(rule => rule.resolution.address_strategy === fixed)
        ? `DNS 策略只解析 ${normal}`
        : `自定义 DNS 本次仅返回 ${normal}，失败处理为停止连接`;
    }
    return {
      dnsPath,
      dnsPart,
      family: job?.family ?? null,
      route,
      service,
      name: DISPLAY_SERVICE_NAMES[service],
      category: CATEGORY[service],
      result,
      source: source ?? null,
      verdict,
      region: result?.region ?? null,
      change: result && previousResult ? changeOf(result, previousResult, service) : null,
      youtube: service === 'youtube' ? (source?.youtube ?? null) : null,
    };
  });
  return {
    rows,
    hasResults: shown !== null,
    shownAt: shownAt ?? null,
    stale: job?.state !== 'obsolete' && (previous !== null || job?.state === 'expired' || running) && shown !== null,
  };
}
