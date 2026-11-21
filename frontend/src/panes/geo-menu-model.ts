// geosite／geoip 选择菜单的检索与分组。只依赖目录数据，不发请求，便于单测。
import type { GeoCatalog, GeoCategory, GeodataKind } from '../api/geodata';
import { REGION_GROUPS } from '../ui/region-groups';

/** 一次渲染的行数上限；更多结果按需展开，避免上千行一次进入 DOM。 */
export const GEO_PAGE = 80;

/**
 * geosite 主题：按服务所在的 `category-*` 族归类。服务归属来自后端精确包含关系（`within`），
 * 这里只把族名映射成中文标签；没有映射的族（如 category-companies）不当作主题。
 */
export const GEO_TOPICS: { id: string; label: string; pattern: RegExp }[] = [
  { id: 'ai', label: 'AI', pattern: /^category-ai/ },
  { id: 'media', label: '影音娱乐', pattern: /^category-(entertainment|media|emby|novel|acg)/ },
  { id: 'games', label: '游戏', pattern: /^category-(games|game-|enhance-gaming|flight-simulator)/ },
  { id: 'dev', label: '开发与云', pattern: /^category-(dev|container|cdn|tech-media)/ },
  { id: 'social', label: '社交通讯', pattern: /^category-(communication|social-media|forums|voip|blog)/ },
  {
    id: 'shop',
    label: '购物与金融',
    pattern:
      /^category-(ecommerce|finance|payment|bank|cryptocurrency|retail|securities|insurance|bourse|betting|logistics|food|travel)/,
  },
  { id: 'edu', label: '学术教育', pattern: /^category-(scholar|education|mooc|cas|wiki|olympiad|documents)/ },
  {
    id: 'net',
    label: '网络工具',
    pattern:
      /^category-(vpnservices|anticensorship|speedtest|ddns|doh|httpdns|netdisk|browser|antivirus|urlshortner|password|remote-control|proxy-tunnels|ntp|stun|ipfs|public-tracker|web-archive|collaborate|android-app|ip-geo)/,
  },
  { id: 'ads', label: '广告与追踪', pattern: /^category-(ads|telemetry|consent)/ },
  { id: 'adult', label: '成人', pattern: /^category-porn/ },
];

// v2fly／Loyalsoldier 约定的地区汇总集：按地区或封锁状态汇总，而不是单个服务。
const REGIONAL = new Set(['cn', 'private', 'gfw', 'greatfire', 'china-list']);
const isRegional = (code: string) => REGIONAL.has(code) || /^(geolocation|tld)-/.test(code);

/** GeoSite 中代表集合而非单个服务的分类（category-*、geolocation-*、tld-*、cn、private）。 */
export const isGeoSiteCollection = (code: string) =>
  code.startsWith('category-') ||
  code.startsWith('geolocation-') ||
  code.startsWith('tld-') ||
  code === 'cn' ||
  code === 'private';

export interface GeoGroup {
  id: string;
  label: string;
  /** 侧栏分隔标题，出现在这一组之前。 */
  heading?: string;
  /** 浏览时的成员顺序；A–Z 等大分组另行分页。 */
  codes: string[];
}

export interface GeoIndex {
  kind: GeodataKind;
  entries: GeoCategory[];
  byCode: Map<string, GeoCategory>;
  /** 每个分类唯一的结构归属：地区汇总、主题、大洲或其他。 */
  home: Map<string, string>;
  groups: GeoGroup[];
}

const regionNames = (() => {
  try {
    return new Intl.DisplayNames(['zh-CN'], { type: 'region' });
  } catch {
    return null;
  }
})();
export const regionName = (code: string) =>
  /^[a-z]{2}$/i.test(code) ? (regionNames?.of(code.toUpperCase()) ?? '') : '';

function topicOf(entry: GeoCategory): string | null {
  const families = entry.code.startsWith('category-') ? [entry.code] : (entry.within ?? []);
  for (const family of families) {
    if (!family.startsWith('category-')) continue;
    const topic = GEO_TOPICS.find(t => t.pattern.test(family));
    if (topic) return topic.id;
  }
  return null;
}

export function geoIndex(kind: GeodataKind, catalog: GeoCatalog, selected: string[]): GeoIndex {
  const entries = catalog.entries;
  const byCode = new Map(entries.map(entry => [entry.code.toLowerCase(), entry]));
  const home = new Map<string, string>();
  const byCount = (a: string, b: string) =>
    (byCode.get(b)?.count ?? 0) - (byCode.get(a)?.count ?? 0) || a.localeCompare(b);
  const used = [
    ...new Set([...selected, ...(catalog.references ?? []).map(r => r.selector)].map(v => v.toLowerCase())),
  ];
  const groups: GeoGroup[] = [];
  const featured = (catalog.featured ?? []).filter(code => byCode.has(code));
  if (featured.length) groups.push({ id: 'featured', label: '常用', codes: featured });
  groups.push({ id: 'used', label: '本条已选与已用', codes: used });
  if (kind === 'geosite') {
    for (const entry of entries)
      home.set(entry.code, isRegional(entry.code) ? 'regional' : (topicOf(entry) ?? 'other'));
    const byGeoSiteOrder = (a: string, b: string) =>
      Number(isGeoSiteCollection(b)) - Number(isGeoSiteCollection(a)) || byCount(a, b);
    const members = (id: string) =>
      entries
        .map(e => e.code)
        .filter(code => home.get(code) === id)
        .sort(byGeoSiteOrder);
    groups.push({ id: 'regional', label: '地区与汇总', codes: members('regional') });
    GEO_TOPICS.forEach((topic, i) =>
      groups.push({
        id: topic.id,
        label: topic.label,
        heading: i === 0 ? '主题（按 category-* 归类）' : undefined,
        codes: members(topic.id),
      }),
    );
    groups.push({ id: 'other', label: '未归类服务', codes: members('other') });
  } else {
    const continent = new Map(REGION_GROUPS.flatMap(g => g.codes.map(code => [code.toLowerCase(), g.id])));
    for (const entry of entries) home.set(entry.code, continent.get(entry.code) ?? 'other');
    REGION_GROUPS.filter(g => g.id !== 'special').forEach((group, i) =>
      groups.push({
        id: group.id,
        label: group.label,
        heading: i === 0 ? '大洲' : undefined,
        codes: entries
          .map(e => e.code)
          .filter(code => home.get(code) === group.id)
          .sort(byCount),
      }),
    );
    groups.push({
      id: 'other',
      label: '其他',
      codes: entries
        .map(e => e.code)
        .filter(code => home.get(code) === 'other')
        .sort(byCount),
    });
  }
  groups.push({
    id: 'az',
    label: 'A–Z',
    heading: '全部',
    codes: entries
      .map(e => e.code)
      .sort((a, b) =>
        kind === 'geosite'
          ? Number(isGeoSiteCollection(b)) - Number(isGeoSiteCollection(a)) || a.localeCompare(b)
          : a.localeCompare(b),
      ),
  });
  return {
    kind,
    entries,
    byCode,
    home,
    groups: groups.filter(g => g.id === 'az' || g.id === 'used' || g.codes.length),
  };
}

export const letterOf = (code: string) => (/^[a-z]/.test(code) ? code[0] : '#');

/** 查询意图：空、按域名反查（仅 geosite）、或按名称与属性检索。 */
export type GeoQuery =
  | { mode: 'empty' }
  | { mode: 'domain'; domain: string }
  | { mode: 'list'; text: string; base: string; attr: string | null };

const HOSTNAME = /^(?=.{1,253}$)([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)(\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+\.?$/;

/** 输入像完整域名（含点、合法标签、不是纯数字的 IP）；大小写已由调用方统一为小写。 */
export const looksLikeDomain = (text: string) => text.includes('.') && HOSTNAME.test(text) && !/^[\d.]+$/.test(text);

export function parseGeoQuery(index: GeoIndex, raw: string): GeoQuery {
  const text = raw.trim().toLowerCase();
  if (!text) return { mode: 'empty' };
  if (index.kind === 'geosite' && looksLikeDomain(text) && !index.byCode.has(text))
    return { mode: 'domain', domain: text.replace(/\.$/, '') };
  const at = text.indexOf('@');
  return {
    mode: 'list',
    text,
    base: (at < 0 ? text : text.slice(0, at)).replace(/^!+/, ''),
    attr: at < 0 ? null : text.slice(at + 1),
  };
}

export interface GeoMatch {
  entry: GeoCategory;
  /** 0 完全相同，1 整段相同（cn → geolocation-cn），2 前缀，3 段前缀，4 包含，5 中文地区名。 */
  tier: number;
  /** 属性检索时命中的属性。 */
  attrs?: string[];
}

/** 名称检索：先按匹配程度分档，同档内条目多的在前（通常是更常用的服务或汇总集）。 */
export function searchGeo(index: GeoIndex, query: Extract<GeoQuery, { mode: 'list' }>): GeoMatch[] {
  const { base, attr } = query;
  const matches: GeoMatch[] = [];
  for (const entry of index.entries) {
    const code = entry.code;
    const parts = code.split(/[-_.]/);
    let tier: number;
    if (!base) tier = 2;
    else if (code === base) tier = 0;
    else if (parts.some(part => part === base || part === `!${base}`)) tier = 1;
    else if (code.startsWith(base)) tier = 2;
    else if (parts.some((part, i) => i > 0 && part.startsWith(base))) tier = 3;
    else if (code.includes(base)) tier = 4;
    else if (index.kind === 'geoip' && regionName(code).includes(base)) tier = 5;
    else continue;
    if (attr === null) {
      matches.push({ entry, tier });
      continue;
    }
    const attrs = entry.attrs.filter(a => a.startsWith(attr));
    if (attrs.length) matches.push({ entry, tier, attrs });
  }
  if (index.kind === 'geosite') {
    return matches.sort((a, b) => {
      const colA = isGeoSiteCollection(a.entry.code);
      const colB = isGeoSiteCollection(b.entry.code);
      return (
        Number(b.tier === 0) - Number(a.tier === 0) ||
        Number(colB && b.tier <= 3) - Number(colA && a.tier <= 3) ||
        a.tier - b.tier ||
        Number(colB) - Number(colA) ||
        b.entry.count - a.entry.count ||
        a.entry.code.localeCompare(b.entry.code)
      );
    });
  }
  return matches.sort(
    (a, b) => a.tier - b.tier || b.entry.count - a.entry.count || a.entry.code.localeCompare(b.entry.code),
  );
}

/** 另有多少分类带有与查询同名的属性（搜 cn 时提示 @cn）。 */
export const attrHits = (index: GeoIndex, base: string) =>
  base ? index.entries.filter(entry => entry.attrs.includes(base)).length : 0;

export interface GeoFamily {
  name: string;
  /** 全部 = 不带地区后缀的同族分类。 */
  variants: { code: string; label: string }[];
}

/** 把 category-ai-cn／category-ai-!cn／category-ai-ru 折叠成一行，按族内最大条目数排序。 */
export function geoFamilies(index: GeoIndex, codes: string[]): GeoFamily[] {
  const families = new Map<string, GeoFamily['variants']>();
  for (const code of codes) {
    const match = /^(category-.+)-(!?[a-z]{2})$/.exec(code);
    const name = match ? match[1] : code;
    const variants = families.get(name) ?? [];
    variants.push({ code, label: match ? `-${match[2]}` : '全部' });
    families.set(name, variants);
  }
  const size = (variants: GeoFamily['variants']) =>
    Math.max(...variants.map(v => index.byCode.get(v.code)?.count ?? 0));
  return [...families]
    .map(([name, variants]) => ({
      name,
      variants: variants.sort((a, b) =>
        a.label === '全部' ? -1 : b.label === '全部' ? 1 : a.label.localeCompare(b.label),
      ),
    }))
    .sort((a, b) => size(b.variants) - size(a.variants) || a.name.localeCompare(b.name));
}

/**
 * 不能加入的原因。与保存时后端的包含校验同源：前面规则或本条已有更宽的分类时，新值不会再命中。
 * `within` 只覆盖不带属性的分类；`x@attr` 另外被 `x` 本身包含。
 */
export function geoBlockReason(
  index: GeoIndex,
  value: string,
  values: string[],
  blockedValues: string[],
): string | null {
  const lower = value.toLowerCase();
  const prior = blockedValues.map(v => v.toLowerCase());
  if (prior.some(v => v === lower || lower.startsWith(`${v}@`))) return '前面规则已有';
  const [code] = lower.split('@');
  const wider = new Set(index.byCode.get(code)?.within ?? []);
  if (lower.includes('@')) wider.add(code);
  if (values.some(v => v.toLowerCase() !== lower && wider.has(v.toLowerCase()))) return '本条已包含';
  if (prior.some(v => wider.has(v))) return '前面规则已包含';
  return null;
}

/** 反查结果中范围很宽的汇总集：条目上万时通常远超用户想匹配的站点。 */
export const GEO_WIDE_COUNT = 10_000;
