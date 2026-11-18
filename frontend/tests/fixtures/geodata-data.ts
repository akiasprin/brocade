import type { GeoCatalog, GeodataKind } from '../../src/api/geodata';
export const geoCatalog = (kind: GeodataKind): GeoCatalog => ({
  source: `https://geodata.example/${kind}.dat`,
  loaded_at: 1794931200,
  entries:
    kind === 'geosite'
      ? [
          { code: 'category-ai-!cn', count: 1250, attrs: [] },
          { code: 'google', count: 102, attrs: ['cn', 'ads'] },
          { code: 'huggingface', count: 3, attrs: [] },
          { code: 'netflix', count: 26, attrs: [] },
          { code: 'openai', count: 6, attrs: [] },
          { code: 'anthropic', count: 3, attrs: [] },
          { code: 'media', count: 35, attrs: [] },
          ...['disney', 'dazn', 'spotify', 'one.example', 'two.example', 'cn', 'dmm', 'tiktok', 'category-ads-all'].map(
            code => ({
              code,
              count: 3,
              attrs: [],
            }),
          ),
        ]
      : [
          { code: 'cn', count: 18000, attrs: ['ipv4', 'ipv6'] },
          { code: 'jp', count: 7200, attrs: ['ipv4', 'ipv6'] },
          { code: 'telegram', count: 16, attrs: ['ipv4'] },
        ],
  references: [{ selector: kind === 'geosite' ? 'category-ai-!cn' : 'cn', location: 'video/stream/hk-akile', rule: 0 }],
});
export function geodataFixture(path: string, init?: RequestInit): Response | null {
  const url = new URL(path, 'http://fixture');
  if (url.pathname === '/model/validate') return Response.json({ valid: true });
  if (url.pathname === '/routing/geodata/catalog')
    return Response.json(geoCatalog(url.searchParams.get('kind') as GeodataKind));
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
    const offset = Number(url.searchParams.get('offset') ?? 0);
    const ip = url.pathname.includes('/geoip/');
    const entries = Array.from({ length: 102 }, (_, i) => ({
      kind: ip ? 'ipv4' : 'domain',
      value: ip ? `192.0.${i}.0/24` : `example-${i}.com`,
      attrs: [],
    }));
    const q = url.searchParams.get('q') ?? '';
    const filtered = entries.filter(e => e.value.includes(q));
    const target = url.searchParams.get('target');
    return Response.json({
      code,
      total: filtered.length,
      counts: { [ip ? 'ipv4' : 'domain']: 102 },
      entries: filtered.slice(offset, offset + 100),
      offset,
      next_offset: offset + 100 < filtered.length ? offset + 100 : null,
      hit: target ? target === 'example-0.com' || target === '192.0.0.1' : null,
    });
  }
  return null;
}
