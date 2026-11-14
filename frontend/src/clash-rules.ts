export type RuleKind = 'domain' | 'keyword' | 'ip' | 'process';
export type Target = 'select' | 'auto' | 'direct' | 'reject' | `line:${string}`;
export type GlyphId =
  | 'rocket'
  | 'bolt'
  | 'fish'
  | 'sparkle'
  | 'house'
  | 'ban'
  | 'globe'
  | 'direct'
  | 'cn'
  | 'youtube'
  | 'google'
  | 'telegram'
  | 'github'
  | 'microsoft'
  | 'apple'
  | 'netflix'
  | 'disney'
  | 'spotify'
  | 'tiktok'
  | 'steam'
  | 'discord'
  | 'twitter'
  | 'instagram'
  | 'reddit'
  | 'twitch'
  | 'whatsapp';

export interface CustomRule {
  id: string;
  kind: RuleKind;
  value: string;
  target: Target;
}

export interface ModuleDef {
  id: string;
  name: string;
  yaml: string;
  glyph: GlyphId;
  standard: Target;
  sets: string[];
  /** 不能关闭（私有网络、国内服务）。 */
  locked?: boolean;
  /** 只在拦截 / 直连之间选（广告拦截）。 */
  blockOnly?: boolean;
  /** 从服务目录添加。 */
  catalog?: boolean;
}

export interface ModuleState {
  on: boolean;
  target: Target;
}

export interface Rules {
  custom: CustomRule[];
  modules: Record<string, ModuleState>;
  /** 从目录添加的服务，按添加顺序排在「非中国」之前。 */
  extra: string[];
  final: Target;
}

export const KIND: Record<RuleKind, { label: string; clash: string; hint: string }> = {
  domain: { label: '域名', clash: 'DOMAIN-SUFFIX', hint: '含全部子域名' },
  keyword: { label: '关键词', clash: 'DOMAIN-KEYWORD', hint: '域名包含这段文字' },
  ip: { label: 'IP 段', clash: 'IP-CIDR', hint: '单个地址按 /32 或 /128' },
  process: { label: '进程', clash: 'PROCESS-NAME', hint: '客户端所在设备上的程序名' },
};
export const KINDS: RuleKind[] = ['domain', 'keyword', 'ip', 'process'];

export interface Line {
  id: Target;
  name: string;
  region: string;
  yaml: string;
}

/** 输入框的一段文字 → 规则类型与规范化后的值；认不出时返回 null。 */
export function detectRule(input: string): { kind: RuleKind; value: string } | null {
  let text = input.trim();
  if (!text) return null;
  const ipv4 = /^(\d{1,3})(\.\d{1,3}){3}(\/\d{1,2})?$/;
  if (ipv4.test(text)) {
    const [address, prefix] = text.split('/');
    if (address.split('.').some(part => Number(part) > 255)) return null;
    if (prefix !== undefined && Number(prefix) > 32) return null;
    return { kind: 'ip', value: prefix === undefined ? `${address}/32` : text };
  }
  if (/^[0-9a-f:]+:[0-9a-f:]*(\/\d{1,3})?$/i.test(text) && text.includes(':')) {
    return { kind: 'ip', value: text.includes('/') ? text : `${text}/128` };
  }
  if (/\.(exe|app)$/i.test(text) && !text.includes('/')) return { kind: 'process', value: text };
  text = text
    .replace(/^[a-z]+:\/\//i, '')
    .replace(/[/?#].*$/, '')
    .replace(/^\*?\./, '')
    .toLowerCase();
  if (/^([a-z0-9-]+\.)+[a-z]{2,}$/.test(text)) return { kind: 'domain', value: text };
  if (/^[a-z0-9-]{2,}$/.test(text)) return { kind: 'keyword', value: text };
  return null;
}

/** 这个类型下的值是否合法（换类型后用）。 */
export function validFor(kind: RuleKind, value: string): boolean {
  const found = detectRule(value);
  if (!found) return false;
  if (kind === 'keyword') return /^[a-z0-9.-]{2,}$/i.test(value.trim());
  if (kind === 'process') return /^[\w .-]+$/.test(value.trim());
  return found.kind === kind;
}

export const sameRules = (a: Rules, b: Rules) => JSON.stringify(a) === JSON.stringify(b);
export const nextId = () => crypto.randomUUID();
export const moduleOrder = (rules: Rules, modules: ModuleDef[]) => {
  const standard = modules.filter(module => !module.catalog);
  return [
    ...standard.filter(module => module.id !== 'global'),
    ...rules.extra.flatMap(id => modules.filter(module => module.id === id)),
    ...standard.filter(module => module.id === 'global'),
  ];
};

export interface RoutingCatalog {
  modules: {
    id: string;
    name: string;
    yaml: string;
    standard: Target;
    sets: string[];
    locked: boolean;
    block_only: boolean;
    catalog: boolean;
  }[];
  lines: { id: Target; name: string; yaml: string }[];
  select: string;
  auto: string;
  final_group: string;
}
