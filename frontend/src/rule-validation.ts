import type { DestMatch, Rule } from './api';
import type { GeoCatalog } from './api/geodata';
import { geoValueIssue } from './api/geodata';
export type MatchProblem = { value: string; message: string };
const valuesOf = (match: DestMatch): string[] =>
  'v' in match && Array.isArray(match.v) && match.t !== 'all'
    ? (match.v as string[])
    : match.t === 'domain_regex'
      ? [match.v]
      : [];
const lowerKinds = new Set(['geosite', 'geoip', 'domain_suffix', 'protocol']);
const covers = (kind: string, a: string, b: string) => {
  if (lowerKinds.has(kind)) {
    a = a.toLowerCase().replace(/\.$/, '');
    b = b.toLowerCase().replace(/\.$/, '');
  }
  return a === b || (kind === 'domain_suffix' && b.endsWith(`.${a}`)) || (kind === 'geosite' && b.startsWith(`${a}@`));
};
export function matchProblem(match: DestMatch): MatchProblem | null {
  if (match.t === 'all') return match.v.map(matchProblem).find(Boolean) ?? null;
  const values = valuesOf(match);
  if ('v' in match && Array.isArray(match.v) && !match.v.length) return { value: '', message: '请填写匹配内容' };
  for (const value of values) {
    let message = '';
    if (!value.trim()) message = '请填写匹配内容';
    else if (
      match.t === 'domain_suffix' &&
      (!/^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)*[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.?$/i.test(value) ||
        value.length > 253)
    )
      message = '不是域名';
    else if (match.t === 'domain_keyword' && /^(full|domain|regexp|geosite|ext|keyword):/i.test(value))
      message = '不能带匹配前缀';
    else if (match.t === 'port' && !/^\d+(?:-\d+)?$/.test(value)) message = '端口或范围无效';
    else if (match.t === 'port') {
      const [first, last = first] = value.split('-').map(Number);
      if (first > last || last > 65535) message = '端口或范围无效';
    } else if (match.t === 'ip_cidr') {
      if (/^(geoip|ext):/i.test(value)) message = 'IP 或网段无效';
      else {
        const [ip, prefix] = value.split('/');
        const v4 = /^(?:\d{1,3}\.){3}\d{1,3}$/.test(ip) && ip.split('.').every(n => Number(n) <= 255);
        const v6 = ip.includes(':') && /^[a-f0-9:]+$/i.test(ip);
        if ((!v4 && !v6) || (prefix !== undefined && (!/^\d+$/.test(prefix) || Number(prefix) > (v4 ? 32 : 128))))
          message = 'IP 或网段无效';
      }
    } else if (
      match.t === 'geoip' &&
      value
        .split('@')
        .slice(1)
        .some(a => !['ipv4', 'ipv6'].includes(a))
    )
      message = '地址族无效';
    else if (match.t === 'domain_regex') {
      if (/\(\?(?:[=!]|<[=!])|\\[1-9]/.test(value)) message = 'RE2 不支持断言或反向引用';
    }
    if (message) return { value, message };
  }
  return null;
}
export function rulesProblems(
  rules: Rule[],
  catalogs: Partial<Record<'geosite' | 'geoip', GeoCatalog>>,
  initial: Rule[] = [],
): Map<number, MatchProblem> {
  const problems = new Map<number, MatchProblem>();
  rules.forEach((rule, i) => {
    const syntax = matchProblem(rule.m);
    if (syntax) {
      problems.set(i, syntax);
      return;
    }
    const values = valuesOf(rule.m);
    for (let at = 0; at < values.length; at++) {
      const value = values[at];
      if (rule.m.t === 'geosite' || rule.m.t === 'geoip') {
        const catalog = catalogs[rule.m.t];
        const invalid = geoValueIssue(catalog, value);
        if (invalid) {
          problems.set(i, { value, message: invalid });
          break;
        }
        const old = initial.some(r => r.m.t === rule.m.t && valuesOf(r.m).includes(value));
        if (!catalog && !old) {
          problems.set(i, { value, message: '规则集未加载' });
          break;
        }
      }
      if (values.some((v, j) => j !== at && covers(rule.m.t, v, value) && (j < at || !covers(rule.m.t, value, v)))) {
        problems.set(i, { value, message: `${value} 本条已包含` });
        break;
      }
      const prior = rules
        .slice(0, i)
        .findIndex(r => r.m.t === rule.m.t && valuesOf(r.m).some(v => covers(rule.m.t, v, value)));
      if (prior >= 0) {
        problems.set(i, { value, message: `${value} 规则 ${prior + 1} 已包含` });
        break;
      }
    }
  });
  return problems;
}
