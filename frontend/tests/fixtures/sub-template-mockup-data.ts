// Synthetic chains, grants and the SubBoost 标准版 group table for the subscription-template mockup. Never imported
// by the app. Group names, rule sets, member order and the rule match order mirror
// crates/brocade-core/src/format/yaml.rs; chains, users and unlock readings are made up.

/* ══ 线路 ══ */

export type Verdict = 'ok' | 'no' | 'unknown';
type Wire = 'vless' | 'anytls' | 'quic';

export interface Chain {
  id: string;
  name: string;
  country: string;
  wires: Wire[];
  v6: boolean;
  /** ChatGPT / Claude / Gemini taken together, from the chain's unlock report. */
  ai: Verdict;
  /** YouTube Premium. */
  premium: { verdict: Verdict; region: string | null };
}

export interface App {
  id: string;
  label: string;
  chains: Chain[];
}

const chain = (
  id: string,
  name: string,
  country: string,
  extra: Partial<Omit<Chain, 'id' | 'name' | 'country'>> = {},
): Chain => ({
  id,
  name,
  country,
  wires: ['vless'],
  v6: false,
  ai: 'ok',
  premium: { verdict: 'ok', region: country },
  ...extra,
});

// prettier-ignore
export const APPS: App[] = [
  {
    id: 'apac',
    label: '亚太精品',
    chains: [
      chain('hk-direct', '香港 直出', 'HK', { wires: ['vless', 'quic'], v6: true, ai: 'no' }),
      chain('jp-iij', '东京 IIJ', 'JP', { wires: ['vless', 'anytls'], v6: true }),
      chain('jp-sb', '东京 SoftBank', 'JP'),
      chain('tw-hinet', '台北 HiNet', 'TW', { v6: true }),
      chain('sg', '新加坡', 'SG'),
      chain('kr-kt', '首尔 KT', 'KR'),
    ],
  },
  {
    id: 'cn2',
    label: '三网优化',
    chains: [
      chain('us-gia', '洛杉矶 CN2 GIA', 'US', { wires: ['vless', 'anytls', 'quic'], v6: true }),
      chain('us-9929', '洛杉矶 9929', 'US'),
      chain('us-cmin2', '洛杉矶 CMIN2', 'US'),
      chain('mo-ctm', '澳门 CTM', 'MO', { ai: 'no', premium: { verdict: 'no', region: 'MO' } }),
    ],
  },
  {
    id: 'west',
    label: '欧美标准',
    chains: [
      chain('us-sjc', '圣何塞', 'US', { v6: true }),
      chain('de-fra', '法兰克福', 'DE'),
      chain('gb-lon', '伦敦 家宽', 'GB'),
    ],
  },
];

export const CHAINS: Chain[] = APPS.flatMap(app => app.chains);
const CHAIN_BY_ID = new Map(CHAINS.map(item => [item.id, item]));
export const chainById = (id: string) => CHAIN_BY_ID.get(id)!;

/* ══ 用户与授权 ══ */

// prettier-ignore
export const GRANTS: Record<string, string[]> = {
  alice: ['hk-direct', 'jp-iij', 'jp-sb', 'tw-hinet', 'sg', 'us-gia', 'us-9929', 'mo-ctm', 'us-sjc', 'gb-lon'],
  bob: ['hk-direct', 'jp-iij', 'jp-sb', 'tw-hinet', 'kr-kt', 'us-gia', 'us-cmin2', 'de-fra'],
  carol: ['jp-iij', 'us-gia'],
  dave: ['hk-direct', 'jp-iij', 'tw-hinet', 'us-gia', 'us-9929'],
  frank: ['hk-direct', 'jp-iij', 'sg', 'us-gia', 'us-sjc'],
  grace: ['hk-direct', 'jp-iij', 'us-gia', 'us-9929'],
  heidi: ['us-gia'],
  ivan: ['hk-direct', 'jp-iij', 'jp-sb', 'tw-hinet', 'us-gia', 'us-cmin2', 'gb-lon'],
  kim: ['hk-direct', 'jp-iij', 'mo-ctm'],
  leo: ['hk-direct', 'jp-iij', 'sg', 'us-cmin2', 'de-fra'],
  mia: ['hk-direct', 'jp-iij', 'jp-sb', 'kr-kt', 'us-gia', 'us-sjc'],
};
export const USERS = Object.keys(GRANTS);
export const SELECTED_USER = 'alice';

/* ══ 模板：SubBoost 标准版的代理组 ══ */

export type GroupId =
  | 'select'
  | 'auto'
  | 'ad'
  | 'ai'
  | 'youtube'
  | 'google'
  | 'microsoft'
  | 'apple'
  | 'telegram'
  | 'github'
  | 'private'
  | 'cn'
  | 'global'
  | 'final';
export type Special = 'select' | 'auto' | 'direct' | 'reject';
export type TargetKey = Special | `chain:${string}`;
/** select = 手动；url-test = 最快；fallback = 按顺序。 */
export type Mode = 'select' | 'url-test' | 'fallback';

export interface GroupConfig {
  on: boolean;
  /** In order; the first is the default of a 手动 group. Empty only for 自动选择, meaning every chain. */
  targets: TargetKey[];
  mode: Mode;
}
export type TemplateConfig = Record<GroupId, GroupConfig>;

export type Glyph =
  | 'rocket'
  | 'bolt'
  | 'ban'
  | 'sparkle'
  | 'youtube'
  | 'google'
  | 'microsoft'
  | 'apple'
  | 'telegram'
  | 'github'
  | 'house'
  | 'cn'
  | 'globe'
  | 'fish';

export interface GroupDef {
  id: GroupId;
  /** The name the client shows, emoji included. The console draws a glyph instead: not every desktop has a colour
   *  emoji font (see ui/region-flag.tsx). */
  yaml: string;
  name: string;
  glyph: Glyph;
  rules: string[];
  /** The (!) note in the group's editor, in the style of 计量口径. */
  note: string;
  /** Service groups can be left out of the template; selector groups and the catch-all cannot. */
  toggle: boolean;
  base: GroupConfig;
  /** Which unlock reading helps choose a chain for this group. */
  unlock?: 'ai' | 'youtube';
}

const follow: GroupConfig = { on: true, targets: ['select'], mode: 'select' };
const direct: GroupConfig = { on: true, targets: ['direct'], mode: 'select' };

// YAML order, which is also the order a client lists the groups in.
// prettier-ignore
export const GROUPS: GroupDef[] = [
  { id: 'select', yaml: '🚀 节点选择', name: '节点选择', glyph: 'rocket', rules: [], note: '手动选择的总出口。跟随节点选择的分组，会随用户在客户端里的选择一起切换。', toggle: false, base: { on: true, targets: ['auto'], mode: 'select' } },
  { id: 'auto', yaml: '⚡ 自动选择', name: '自动选择', glyph: 'bolt', rules: [], note: '每 5 分钟测一次延迟，自动使用最快的线路，供节点选择和其他分组使用。', toggle: false, base: { on: true, targets: [], mode: 'url-test' } },
  { id: 'ad', yaml: '🛑 广告拦截', name: '广告拦截', glyph: 'ban', rules: ['广告域名'], note: '命中广告域名的请求默认被拦截；改为直连或节点选择即不再拦截。', toggle: true, base: { on: true, targets: ['reject'], mode: 'select' } },
  { id: 'ai', yaml: '🤖 AI 服务', name: 'AI 服务', glyph: 'sparkle', rules: ['OpenAI', 'Anthropic', 'AI 对话'], note: 'OpenAI、Anthropic 与其他非中国 AI 对话服务。关闭后，这些流量按后面的规则继续匹配。', toggle: true, base: follow, unlock: 'ai' },
  { id: 'youtube', yaml: '📹 油管视频', name: '油管视频', glyph: 'youtube', rules: ['YouTube'], note: 'YouTube 网页、App 与视频流。关闭后，这些流量按后面的规则继续匹配。', toggle: true, base: follow, unlock: 'youtube' },
  { id: 'google', yaml: '🔍 谷歌服务', name: '谷歌服务', glyph: 'google', rules: ['Google', 'Google IP'], note: 'Google 域名与 IP 段。关闭后，这些流量按后面的规则继续匹配。', toggle: true, base: follow },
  { id: 'microsoft', yaml: 'Ⓜ️ 微软服务', name: '微软服务', glyph: 'microsoft', rules: ['Microsoft', 'OneDrive'], note: 'Microsoft 与 OneDrive。关闭后，这些流量按后面的规则继续匹配。', toggle: true, base: follow },
  { id: 'apple', yaml: '🍏 苹果服务', name: '苹果服务', glyph: 'apple', rules: ['Apple', 'iCloud'], note: 'Apple 与 iCloud。关闭后，这些流量按后面的规则继续匹配。', toggle: true, base: follow },
  { id: 'telegram', yaml: '📲 电报消息', name: '电报消息', glyph: 'telegram', rules: ['Telegram', 'Telegram IP'], note: 'Telegram 域名与 IP 段。关闭后，这些流量按后面的规则继续匹配。', toggle: true, base: follow },
  { id: 'github', yaml: '🐱 代码托管', name: '代码托管', glyph: 'github', rules: ['GitHub', 'GitLab', 'Atlassian'], note: 'GitHub、GitLab 与 Atlassian。关闭后，这些流量按后面的规则继续匹配。', toggle: true, base: follow },
  { id: 'private', yaml: '🏠 私有网络', name: '私有网络', glyph: 'house', rules: ['局域网域名', '私有 IP'], note: '局域网域名与私有 IP，默认直连，不能关闭。', toggle: false, base: direct },
  { id: 'cn', yaml: '🔒 国内服务', name: '国内服务', glyph: 'cn', rules: ['中国域名', '中国 IP'], note: '中国域名与中国 IP，默认直连，不能关闭。', toggle: false, base: direct },
  { id: 'global', yaml: '🌍 非中国', name: '非中国', glyph: 'globe', rules: ['非中国域名'], note: '前面规则没有命中的非中国域名。关闭后，这些流量交给漏网之鱼。', toggle: true, base: follow },
  { id: 'final', yaml: '🐟 漏网之鱼', name: '漏网之鱼', glyph: 'fish', rules: ['其余流量'], note: '前面的规则都没有命中的流量走这里，是整份订阅的兜底。', toggle: false, base: follow },
];
export const groupById = (id: GroupId) => GROUPS.find(group => group.id === id)!;

/** First match wins, top to bottom; 漏网之鱼 is the final MATCH. */
export const RULE_ORDER: GroupId[] = [
  'ad',
  'private',
  'ai',
  'cn',
  'youtube',
  'google',
  'telegram',
  'github',
  'microsoft',
  'apple',
  'global',
  'final',
];

export type FinalPlace = 'third' | 'bottom';
export interface Section {
  id: 'core' | 'service' | 'direct' | 'final';
  label: string;
  groups: GroupId[];
}
const SERVICE_GROUPS: GroupId[] = ['ai', 'youtube', 'google', 'microsoft', 'apple', 'telegram', 'github', 'global'];

/** 节点选择、自动选择永远在最前；漏网之鱼排第三，或单独放在最下面。 */
export const sectionsFor = (place: FinalPlace): Section[] => [
  { id: 'core', label: '通用', groups: place === 'third' ? ['select', 'auto', 'final'] : ['select', 'auto'] },
  { id: 'service', label: '服务分流', groups: SERVICE_GROUPS },
  { id: 'direct', label: '直连与拦截', groups: ['ad', 'private', 'cn'] },
  ...(place === 'bottom' ? [{ id: 'final' as const, label: '兜底', groups: ['final' as GroupId] }] : []),
];
export const SPECIAL_LABEL: Record<Special, string> = {
  select: '节点选择',
  auto: '自动选择',
  direct: '直连',
  reject: '拦截',
};
export const MODE_LABEL: Record<Mode, string> = { select: '手动', 'url-test': '最快', fallback: '按顺序' };

export const isChainKey = (key: TargetKey): key is `chain:${string}` => key.startsWith('chain:');
export const chainOfKey = (key: `chain:${string}`) => chainById(key.slice('chain:'.length));
export const chainKey = (id: string): TargetKey => `chain:${id}`;

export const baseTemplate = (): TemplateConfig =>
  Object.fromEntries(
    GROUPS.map(group => [group.id, { ...group.base, targets: [...group.base.targets] }]),
  ) as TemplateConfig;

export const sameConfig = (left: GroupConfig, right: GroupConfig) =>
  left.on === right.on &&
  left.mode === right.mode &&
  left.targets.length === right.targets.length &&
  left.targets.every((target, index) => target === right.targets[index]);

export const isBase = (id: GroupId, config: GroupConfig) => sameConfig(config, groupById(id).base);

export type SceneKey = 'tuned' | 'fresh';
export const SCENES: Record<SceneKey, { name: string; template: () => TemplateConfig }> = {
  tuned: {
    name: '已调整 4 组',
    template: () => ({
      ...baseTemplate(),
      final: { on: true, targets: [chainKey('hk-direct'), chainKey('jp-iij')], mode: 'fallback' },
      ai: { on: true, targets: [chainKey('us-gia'), chainKey('jp-iij')], mode: 'select' },
      youtube: { on: true, targets: [chainKey('hk-direct')], mode: 'select' },
      microsoft: { on: false, targets: ['select'], mode: 'select' },
    }),
  },
  fresh: { name: '默认模板', template: baseTemplate },
};

export const changedGroups = (template: TemplateConfig) =>
  GROUPS.filter(group => !isBase(group.id, template[group.id]));

/* ══ 按用户落地 ══ */

/** What a user actually gets: chains they hold, plus every non-chain target. Empty means the group keeps its
 *  standard composition, i.e. it follows 节点选择. */
export const effectiveTargets = (config: GroupConfig, user: string) =>
  config.targets.filter(target => !isChainKey(target) || GRANTS[user].includes(target.slice('chain:'.length)));

const flagEmoji = (code: string) =>
  String.fromCodePoint(...[...code].map(letter => 0x1f1e6 + letter.charCodeAt(0) - 65));

/** Entry names exactly as `project_user` renders them: `{flag}{chain}{ | protocol}{ | v6}`. */
export const entryNames = (item: Chain) => {
  const base = `${flagEmoji(item.country)}${item.name}`;
  return item.wires.flatMap(wire => {
    const name = `${base}${wire === 'anytls' ? ' | AnyTLS' : wire === 'quic' ? ' | QUIC' : ''}`;
    return item.v6 ? [name, `${name} | v6`] : [name];
  });
};

const userEntries = (user: string) => CHAINS.filter(item => GRANTS[user].includes(item.id)).flatMap(entryNames);

const SPECIAL_YAML: Record<Special, string> = {
  select: '🚀 节点选择',
  auto: '⚡ 自动选择',
  direct: 'DIRECT',
  reject: 'REJECT',
};

const quote = (value: string) => (/^[A-Z]+$/.test(value) ? value : `"${value}"`);

/** The `proxy-groups` item one user receives. An untouched group is byte-for-byte today's output. */
export function groupYaml(def: GroupDef, config: GroupConfig, user: string): string[] {
  if (!config.on) return [`# ${def.yaml}：已关闭，不生成分组与规则`];
  const all = userEntries(user);
  const effective = effectiveTargets(config, user);
  const standard =
    isBase(def.id, config) || effective.length === 0 || (def.id === 'auto' && config.targets.length === 0);
  let type: string = config.mode;
  let members: string[];
  if (standard) {
    const base = def.base;
    type = base.mode;
    if (def.id === 'auto') members = all;
    else if (def.id === 'select') members = [SPECIAL_YAML.auto, 'DIRECT', 'REJECT', ...all];
    else if (base.targets[0] === 'reject') members = ['REJECT', 'DIRECT', SPECIAL_YAML.select];
    else if (base.targets[0] === 'direct')
      members = ['DIRECT', 'REJECT', SPECIAL_YAML.select, SPECIAL_YAML.auto, ...all];
    else members = [SPECIAL_YAML.select, SPECIAL_YAML.auto, 'DIRECT', 'REJECT', ...all];
  } else {
    members = effective.flatMap(target =>
      isChainKey(target) ? entryNames(chainOfKey(target)) : [SPECIAL_YAML[target]],
    );
    // 手动组留一个出口：所选线路都不可用时，客户端里仍能切回节点选择或直连。节点选择组本身列出全部条目。
    if (config.mode === 'select')
      members.push(
        ...(def.id === 'select' ? [SPECIAL_YAML.auto, 'DIRECT', 'REJECT', ...all] : [SPECIAL_YAML.select, 'DIRECT']),
      );
    members = [...new Set(members)];
  }
  const lines = [`  - name: "${def.yaml}"`, `    type: ${type}`, `    proxies: [${members.map(quote).join(', ')}]`];
  if (type !== 'select') {
    lines.push('    url: "https://www.gstatic.com/generate_204"', '    interval: 300');
    if (type === 'url-test') lines.push('    lazy: false');
  }
  return lines;
}
