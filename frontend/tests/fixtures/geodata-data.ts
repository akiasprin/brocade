import type { GeoCatalog, GeodataKind } from '../../src/api/geodata';
export const geoCatalog = (kind: GeodataKind): GeoCatalog => ({
  source: `https://geodata.example/${kind}.dat`,
  loaded_at: 1794931200,
  entries:
    kind === 'geosite'
      ? [
          { code: 'category-ai-!cn', count: 1250, attrs: [] },
          { code: 'category-ai-cn', count: 40, attrs: [] },
          { code: 'google', count: 102, attrs: ['ads', 'cn'], attr_counts: { ads: 12, cn: 30 } },
          { code: 'huggingface', count: 3, attrs: [], within: ['category-ai-!cn'] },
          { code: 'netflix', count: 26, attrs: [], within: ['category-entertainment'] },
          { code: 'openai', count: 6, attrs: [], within: ['category-ai-!cn'] },
          { code: 'anthropic', count: 3, attrs: [], within: ['category-ai-!cn'] },
          { code: 'media', count: 35, attrs: [] },
          { code: 'category-entertainment', count: 400, attrs: [] },
          { code: 'geolocation-!cn', count: 27000, attrs: [] },
          ...['disney', 'dazn', 'spotify', 'one.example', 'two.example', 'cn', 'dmm', 'tiktok', 'category-ads-all'].map(
            code => ({
              code,
              count: 3,
              attrs: [],
            }),
          ),
        ]
      : [
          { code: 'cn', count: 18000, attrs: ['ipv4', 'ipv6'], attr_counts: { ipv4: 12000, ipv6: 6000 } },
          { code: 'jp', count: 7200, attrs: ['ipv4', 'ipv6'], attr_counts: { ipv4: 5000, ipv6: 2200 } },
          { code: 'telegram', count: 16, attrs: ['ipv4'], attr_counts: { ipv4: 16 } },
        ],
  ...(kind === 'geosite' ? { featured: ['category-ads-all', 'openai', 'anthropic', 'google', 'netflix'] } : {}),
  references: [{ selector: kind === 'geosite' ? 'category-ai-!cn' : 'cn', location: 'video/stream/hk-akile', rule: 0 }],
});
export function geodataFixture(path: string, init?: RequestInit): Response | null {
  const url = new URL(path, 'http://fixture');
  if (url.pathname === '/model/validate') return Response.json({ valid: true });
  if (url.pathname === '/routing/geodata/catalog')
    return Response.json(geoCatalog(url.searchParams.get('kind') as GeodataKind));
  if (url.pathname === '/routing/geodata/lookup') {
    const domain = url.searchParams.get('domain') ?? '';
    const hits = domain.endsWith('openai.com')
      ? [
          { code: 'openai', count: 6, kind: 'domain', value: 'openai.com' },
          { code: 'category-ai-!cn', count: 1250, kind: 'domain', value: 'openai.com' },
          { code: 'geolocation-!cn', count: 27000, kind: 'domain', value: 'openai.com' },
        ]
      : [];
    return Response.json({ domain, hits, unknown: [] });
  }
  if (url.pathname === '/routing/geodata/relations') {
    const { values } = JSON.parse(String(init?.body)) as { values: string[] };
    const known = [
      { wider: 'category-ai-!cn', narrower: 'openai' },
      { wider: 'category-ai-!cn', narrower: 'anthropic' },
      { wider: 'category-ai-!cn', narrower: 'huggingface' },
      { wider: 'google', narrower: 'google@cn' },
      { wider: 'google', narrower: 'google@ads' },
    ];
    return Response.json({
      covers: known.filter(p => values.includes(p.wider) && values.includes(p.narrower)),
      errors: {},
    });
  }
  if (url.pathname.startsWith('/routing/geodata/geosite/') || url.pathname.startsWith('/routing/geodata/geoip/')) {
    const code = decodeURIComponent(url.pathname.split('/').at(-1)!);
    const ip = url.pathname.includes('/geoip/');
    const params = url.searchParams;
    const offset = Number(params.get('offset') ?? 0);
    const limit = Number(params.get('limit') ?? 100);
    const attr = params.get('attr') ?? '';
    const type = params.get('type') ?? '';
    const q = (params.get('q') ?? '').toLowerCase();
    const target = (params.get('target') ?? '').toLowerCase();
    // 与 Console 相同：先按属性过滤，再按类型排序（geosite 后缀→完整→正则，geoip IPv4→IPv6），节内保持文件顺序。
    const source = ip ? geoipEntries : code === 'geolocation-!cn' ? longEntries : geositeEntries;
    const rows = source.filter(e => !attr || e.attrs.includes(attr));
    const counts: Record<string, number> = {};
    for (const e of rows) counts[e.kind] = (counts[e.kind] ?? 0) + 1;
    const hits = target ? rows.filter(e => fixtureMatches(e, target, ip)).slice(0, 8) : [];
    const filtered = rows.filter(e => (!type || e.kind === type) && e.value.toLowerCase().includes(q));
    const entries = filtered.slice(offset, offset + limit);
    return Response.json({
      code,
      total: filtered.length,
      counts,
      entries,
      offset,
      next_offset: offset + entries.length < filtered.length ? offset + entries.length : null,
      hit: target ? hits.length > 0 : null,
      hits,
    });
  }
  return null;
}

type FixtureEntry = { kind: string; value: string; attrs: string[] };
/** 102 条：80 条后缀（其中 30 条 @cn）、20 条完整（其中 12 条 @ads）、2 条正则，与目录中 google 的计数一致。 */
const geositeEntries: FixtureEntry[] = Array.from({ length: 102 }, (_, i) =>
  i < 80
    ? { kind: 'domain', value: `example-${i}.com`, attrs: i >= 50 ? ['cn'] : [] }
    : i < 100
      ? { kind: 'full', value: `www.example-${i}.net`, attrs: i < 92 ? ['ads'] : [] }
      : { kind: 'regexp', value: `^cdn\\d+\\.example-${i}\\.org$`, attrs: [] },
);
/** 超过一页（200 条）的分类，用来验证滚动加载。 */
const longEntries: FixtureEntry[] = Array.from({ length: 450 }, (_, i) => ({
  kind: 'domain',
  value: `site-${i}.example`,
  attrs: [],
}));
const geoipEntries: FixtureEntry[] = Array.from({ length: 102 }, (_, i) =>
  i < 90
    ? { kind: 'ipv4', value: `192.0.${i}.0/24`, attrs: [] }
    : { kind: 'ipv6', value: `2001:db8:${i.toString(16)}::/48`, attrs: [] },
);
function fixtureMatches(entry: FixtureEntry, target: string, ip: boolean) {
  if (ip) return entry.kind === 'ipv4' && target.startsWith(entry.value.split('.').slice(0, 3).join('.') + '.');
  if (entry.kind === 'domain') return target === entry.value || target.endsWith(`.${entry.value}`);
  if (entry.kind === 'full') return target === entry.value;
  return new RegExp(entry.value).test(target);
}
