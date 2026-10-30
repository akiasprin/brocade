import type { UnlockConnectionObservation, UnlockPart, UnlockResult, YoutubePageResult } from '../../src/api';

export type DialScenario = 'no-egress' | 'unreachable' | 'mixed';

export function withDialFailure(source: UnlockResult, scenario: DialScenario): UnlockResult {
  const result = structuredClone(source);
  const skipped = scenario === 'no-egress' || (scenario === 'mixed' && source.service !== 'youtube_premium');
  const parts: UnlockPart[] = result.service === 'youtube_premium' ? ['webpage', 'premium', 'cdn'] : ['service'];
  const count = skipped || scenario === 'mixed' ? 1 : 2;
  result.status = skipped ? 'skipped' : 'failed';
  result.code = skipped ? 'no_ipv6_egress' : 'network_error';
  result.region = null;
  result.client_dns = false;
  result.dns_path = null;
  result.connection_requests = Object.fromEntries(parts.map(part => [part, count]));
  const prefix = Array.from(result.service.slice(0, 12), letter => letter.charCodeAt(0).toString(16).padStart(2, '0')).join('').padEnd(24, '0');
  const hosts: Record<string, string> = { chatgpt: 'api.openai.com', claude: 'claude.ai', gemini: 'gemini.google.com', netflix: 'www.netflix.com', disney_plus: 'www.disneyplus.com', tiktok: 'www.tiktok.com', prime_video: 'www.primevideo.com', reddit: 'www.reddit.com', spotify: 'open.spotify.com', hbo_max: 'www.max.com', dazn: 'startup.core.indazn.com', bbc_iplayer: 'open.live.bbc.co.uk', crunchyroll: 'www.crunchyroll.com', paramount_plus: 'www.paramountplus.com' };
  result.connection_observations = parts.flatMap((part, index): UnlockConnectionObservation[] =>
    Array.from({ length: count }, (_, attempt) => {
      const success = scenario === 'mixed' && part !== 'webpage' && part !== 'service';
      const domain = part === 'cdn' ? 'redirector.googlevideo.com' : result.service === 'youtube_premium' ? 'www.youtube.com' : hosts[result.service];
      return {
        part, version: 2, id: prefix + (index * 2 + attempt + 1).toString(16).padStart(8, '0'),
        domain,
        exit: success ? 'ipv6-exit' : 'exit-without-ipv6', outbound: 'out:egress:probe-v6',
        profile: 'a'.repeat(64), normal_strategy: 'UseIP', family: 'v6', connected: success, complete: true,
        error: success ? undefined : skipped || scenario === 'mixed' ? 'ipv6_no_address' : 'network_unreachable',
        lookups: [{ version: 1, domain, strategy: 'FORCE_IP6', complete: true, steps: [{ tag: 'dns-out', scoped: false, outcome: 'answer', other: false }] }],
      };
    }),
  );
  result.dns_parts = parts.map(part => ({ part, kind: 'direct', paths: [], exits: [scenario === 'mixed' && part !== 'webpage' && part !== 'service' ? 'ipv6-exit' : 'exit-without-ipv6'] }));
  if (result.youtube) {
    const page: YoutubePageResult = { status: result.status, code: result.code, http_status: null, page_region: null, topbar_region: null, privacy_region: null };
    result.youtube.webpage = { ...page };
    result.youtube.premium = { ...page };
    result.youtube.cdn = { http_status: null, node: null, airport: null, country: null, error: result.code, elapsed_ms: 3 };
    if (scenario === 'mixed') {
      result.status = 'available'; result.code = 'web_available'; result.region = 'JP';
      result.youtube.webpage = { ...page, status: 'skipped', code: 'no_ipv6_egress' };
      result.youtube.premium = { status: 'available', code: 'web_available', http_status: 200, page_region: 'JP', topbar_region: 'JP', privacy_region: 'JP' };
      result.youtube.cdn = { http_status: 200, node: 'nrt12s28', airport: 'NRT', country: 'JP', error: null, elapsed_ms: 3 };
    }
  }
  return result;
}
