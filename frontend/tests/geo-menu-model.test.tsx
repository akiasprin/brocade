import { describe, expect, it } from 'vitest';
import type { GeoCatalog } from '../src/api/geodata';
import {
  geoBlockReason,
  geoFamilies,
  geoIndex,
  parseGeoQuery,
  searchGeo,
  type GeoQuery,
} from '../src/panes/geo-menu-model';

// 结构取自真实 geosite.dat 的片段：条目数、属性与包含关系都按 Loyalsoldier 文件填写。
const site: GeoCatalog = {
  source: 'https://geodata.example/geosite.dat',
  loaded_at: 1,
  featured: ['category-ads-all', 'openai', 'missing-in-file'],
  entries: [
    { code: 'cn', count: 110255, attrs: [] },
    { code: 'cnn', count: 9, attrs: [] },
    { code: 'cnbeta', count: 3, attrs: [] },
    { code: 'geolocation-cn', count: 5452, attrs: ['cn'], attr_counts: { cn: 40 } },
    { code: 'geolocation-!cn', count: 27509, attrs: [] },
    { code: 'tld-!cn', count: 829, attrs: [] },
    { code: 'acer', count: 9, attrs: ['cn'], attr_counts: { cn: 2 } },
    { code: 'category-ai-!cn', count: 219, attrs: [], within: ['category-ai-chat-!cn'] },
    { code: 'category-ai-chat-!cn', count: 219, attrs: [], within: ['category-ai-!cn'] },
    { code: 'category-ai-cn', count: 129, attrs: [] },
    { code: 'category-ads-all', count: 200374, attrs: [] },
    { code: 'category-companies', count: 8681, attrs: [] },
    { code: 'openai', count: 25, attrs: ['ads'], within: ['category-ai-!cn', 'category-ai-chat-!cn'] },
    { code: 'google', count: 1080, attrs: ['ads', 'cn'], within: ['alphabet', 'category-companies'] },
    { code: 'alphabet', count: 1090, attrs: [], within: ['category-companies'] },
    { code: 'netflix', count: 28, attrs: [], within: ['category-entertainment'] },
    { code: 'category-entertainment', count: 2175, attrs: [] },
  ],
  references: [{ selector: 'netflix', location: 'dns/hk', rule: 0 }],
};
const list = (query: GeoQuery) => {
  if (query.mode !== 'list') throw new Error(`expected a name query, got ${query.mode}`);
  return query;
};

describe('geosite menu model', () => {
  const index = geoIndex('geosite', site, ['openai']);
  const groups = Object.fromEntries(index.groups.map(g => [g.id, g.codes]));

  it('groups by curated choices, usage, regional sets and category topics', () => {
    expect(groups.featured).toEqual(['category-ads-all', 'openai']);
    expect(groups.used).toEqual(['openai', 'netflix']);
    expect(groups.regional).toEqual(['cn', 'geolocation-!cn', 'geolocation-cn', 'tld-!cn']);
    // 服务按最窄的 category-* 归入主题；category-companies 不是主题，所以 google 未归类。
    expect(groups.ai).toEqual(['category-ai-!cn', 'category-ai-chat-!cn', 'category-ai-cn', 'openai']);
    expect(groups.media).toEqual(['category-entertainment', 'netflix']);
    expect(groups.other).toContain('google');
    expect(groups.az).toHaveLength(site.entries.length);
  });

  it('ranks exact names, then whole words, before prefixes and substrings', () => {
    const cn = searchGeo(index, list(parseGeoQuery(index, 'cn'))).map(m => m.entry.code);
    expect(cn.slice(0, 4)).toEqual(['cn', 'geolocation-!cn', 'geolocation-cn', 'tld-!cn']);
    expect(cn.indexOf('cnn')).toBeGreaterThan(cn.indexOf('category-ai-cn'));
    expect(cn).not.toContain('acer');
    expect(searchGeo(index, list(parseGeoQuery(index, 'GOOGLE')))[0].entry.code).toBe('google');
  });

  it('lists attributes after @ and across categories with a bare @attr', () => {
    expect(searchGeo(index, list(parseGeoQuery(index, 'google@'))).map(m => m.attrs)).toEqual([['ads', 'cn']]);
    expect(searchGeo(index, list(parseGeoQuery(index, '@cn'))).map(m => m.entry.code)).toEqual([
      'geolocation-cn',
      'google',
      'acer',
    ]);
  });

  it('treats hostnames as reverse lookups unless they name a category', () => {
    expect(parseGeoQuery(index, ' Chat.OpenAI.com. ')).toEqual({ mode: 'domain', domain: 'chat.openai.com' });
    expect(parseGeoQuery(index, '192.0.2.1').mode).toBe('list');
    expect(parseGeoQuery(index, 'openai').mode).toBe('list');
    const dotted = geoIndex('geosite', { ...site, entries: [{ code: 'one.example', count: 1, attrs: [] }] }, []);
    expect(parseGeoQuery(dotted, 'one.example').mode).toBe('list');
    expect(parseGeoQuery(geoIndex('geoip', site, []), 'chat.openai.com').mode).toBe('list');
  });

  it('folds regional variants of one category family', () => {
    expect(geoFamilies(index, ['category-ai-cn', 'category-ai-!cn', 'category-ai-chat-!cn'])).toEqual([
      {
        name: 'category-ai',
        variants: [
          { code: 'category-ai-!cn', label: '-!cn' },
          { code: 'category-ai-cn', label: '-cn' },
        ],
      },
      { name: 'category-ai-chat', variants: [{ code: 'category-ai-chat-!cn', label: '-!cn' }] },
    ]);
  });

  it('blocks values already covered by this rule or an earlier one', () => {
    const covered = geoIndex('geosite', site, ['category-ai-!cn']);
    expect(geoBlockReason(covered, 'openai', ['category-ai-!cn'], [])).toBe('本条已包含');
    expect(geoBlockReason(covered, 'category-ai-chat-!cn', ['category-ai-!cn'], [])).toBe('本条已包含');
    expect(geoBlockReason(covered, 'google@cn', ['google'], [])).toBe('本条已包含');
    expect(geoBlockReason(covered, 'google', [], ['alphabet'])).toBe('前面规则已包含');
    expect(geoBlockReason(covered, 'google@ads', [], ['google'])).toBe('前面规则已有');
    expect(geoBlockReason(covered, 'netflix', ['category-ai-!cn'], ['openai'])).toBeNull();
  });

  it('groups geoip codes by continent and keeps non-country lists apart', () => {
    const ip = geoIndex(
      'geoip',
      {
        source: 'https://geodata.example/geoip.dat',
        loaded_at: 1,
        entries: ['cn', 'jp', 'us', 'de', 'private', 'telegram'].map((code, i) => ({ code, count: 10 - i, attrs: [] })),
      },
      ['!cn'],
    );
    const ipGroups = Object.fromEntries(ip.groups.map(g => [g.id, g.codes]));
    expect(ipGroups.used).toEqual(['!cn']);
    expect(ipGroups.asia).toEqual(['cn', 'jp']);
    expect(ipGroups.europe).toEqual(['de']);
    expect(ipGroups['north-america']).toEqual(['us']);
    expect(ipGroups.other).toEqual(['private', 'telegram']);
    expect(searchGeo(ip, list(parseGeoQuery(ip, '日本')))[0].entry.code).toBe('jp');
  });
});
