import type { DestMatch } from '../../api';
import type { GeositeChecks } from '../../api/geosite';
import type { FlowModel } from './model';

export interface FlowTarget {
  host: string;
  ip: boolean;
  port: number | null;
  network: 'tcp' | 'udp';
}
export function parseFlowTarget(raw: string, network: 'tcp' | 'udp'): FlowTarget | null {
  const value = raw.trim();
  if (!value || value.length > 512 || /[\s/@?#]/.test(value)) return null;
  let host = value;
  let port: number | null = null;
  const socket = value.match(/^\[([^\]]+)\](?::(\d+))?$/) ?? value.match(/^([^:]+):(\d+)$/);
  if (socket) {
    host = socket[1];
    port = socket[2] ? Number(socket[2]) : null;
  }
  if (port !== null && (!Number.isInteger(port) || port < 1 || port > 65535)) return null;
  try {
    // URL validates IPv6 and converts international domain names; numeric IPv4 shorthand is
    // deliberately rejected because it is too easy to mistake a typo for a different address.
    const normalized = new URL(`http://${host.includes(':') ? `[${host}]` : host}`).hostname.replace(/^\[|\]$/g, '');
    const ip = normalized.includes(':') || /^\d+\.\d+\.\d+\.\d+$/.test(normalized);
    if (ip && !host.includes(':') && normalized !== host) return null;
    if (
      !ip &&
      (!/^[\da-z.-]+$/i.test(normalized) ||
        normalized
          .split('.')
          .some(label => !label || label.length > 63 || label.startsWith('-') || label.endsWith('-')))
    )
      return null;
    return { host: normalized.toLowerCase().replace(/\.$/, ''), ip, port, network };
  } catch {
    return null;
  }
}

type Check = { hit: boolean | null; reason: string; pending?: boolean };
type GeositeState = { matches?: GeositeChecks; error?: string };

export function flowGeositeSets(model: FlowModel): string[] {
  const visit = (match: DestMatch): string[] =>
    match.t === 'geosite' ? match.v : match.t === 'all' ? match.v.flatMap(visit) : [];
  return [...new Set(model.machines.flatMap(machine => machine.rules.flatMap(row => visit(row.rule.m))))].sort();
}

function checkMatch(match: DestMatch, target: FlowTarget, geosite: GeositeState): Check {
  const domain = !target.ip;
  const unknownDomain: Check = { hit: null, reason: 'IP 目标仍可能嗅探出域名，需节点嗅探结果' };
  switch (match.t) {
    case 'any':
      return { hit: true, reason: '任意流量' };
    case 'domain_suffix':
      if (!domain) return unknownDomain;
      return {
        hit:
          domain &&
          match.v.some(
            suffix => target.host === suffix.toLowerCase() || target.host.endsWith(`.${suffix.toLowerCase()}`),
          ),
        reason: '域名后缀',
      };
    case 'domain_keyword':
      if (!domain) return unknownDomain;
      return {
        hit: domain && match.v.some(keyword => target.host.includes(keyword.toLowerCase())),
        reason: '域名关键词',
      };
    case 'domain_regex':
      return domain ? { hit: null, reason: '需由 Xray 正则引擎确认' } : unknownDomain;
    case 'geosite': {
      if (!domain) return unknownDomain;
      const checks = match.v.map(set => geosite.matches?.[set]);
      if (checks.some(check => check?.hit === true)) return { hit: true, reason: '统一 geosite 规则集' };
      const unknown = checks.findIndex(check => check?.hit !== false);
      if (unknown >= 0)
        return {
          hit: null,
          reason: checks[unknown]?.reason ?? geosite.error ?? '正在加载统一 geosite 规则集',
          pending: !geosite.matches && !geosite.error,
        };
      return { hit: false, reason: '统一 geosite 规则集' };
    }
    case 'geoip':
    case 'ip_cidr':
      return { hit: null, reason: '需节点目标 IP、解析策略及实际规则集' };
    case 'sniffing_failed':
      return { hit: domain ? false : null, reason: '纯 IP 目标不等于嗅探失败，需节点嗅探结果' };
    case 'network':
      return { hit: match.v === target.network, reason: `传输层 ${target.network.toUpperCase()}` };
    case 'port':
      return {
        hit:
          target.port === null
            ? null
            : match.v.some(value => {
                const [start, end = start] = value.split('-').map(Number);
                return target.port! >= start && target.port! <= end;
              }),
        reason: target.port === null ? '请在目标后补充端口' : `端口 ${target.port}`,
      };
    case 'all': {
      const checks = match.v.map(item => checkMatch(item, target, geosite));
      return (
        checks.find(check => check.hit === false) ??
        checks.find(check => check.hit === null) ?? { hit: true, reason: '全部条件满足' }
      );
    }
  }
}

export interface FlowTrace {
  rules: Set<string>;
  machines: Set<string>;
  hops: Set<string>;
  uncertain: string | null;
  reason: string | null;
  pending: boolean;
  message: string;
}

/** A conservative preview of configuration facts, never a substitute for runtime Xray tracing.
 * Stop at the first unknown: falling through would turn an unknown earlier rule into a false miss. */
export function traceFlow(model: FlowModel, target: FlowTarget, geosite: GeositeState = {}): FlowTrace {
  const result: FlowTrace = {
    rules: new Set(),
    machines: new Set(),
    hops: new Set(),
    uncertain: null,
    reason: null,
    pending: false,
    message: '',
  };
  let at = model.entry;
  while (at) {
    if (result.machines.has(at)) {
      result.message = '检测到循环引用，追踪已停止';
      return result;
    }
    result.machines.add(at);
    const machine = model.machines.find(item => item.id === at);
    if (!machine) {
      result.message = '缺少目标规则表';
      return result;
    }
    let next: string | null = null;
    for (const row of machine.rules) {
      const check = checkMatch(row.rule.m, target, geosite);
      if (check.hit === false) continue;
      if (check.hit === null) {
        result.uncertain = row.id;
        result.reason = check.reason;
        result.pending = check.pending ?? false;
        result.message = `${machine.name} · 规则 ${row.index + 1}：${check.reason}，尚不能确定后续去向`;
        return result;
      }
      result.rules.add(row.id);
      if (row.target) {
        next = row.target;
        if (row.hop) result.hops.add(row.hop.id);
        break;
      }
      result.message = `${machine.name} · ${row.rule.a.t === 'egress' ? '本机出网' : row.rule.a.t === 'proxy' ? '外部代理' : '拒绝'}（配置预览）`;
      return result;
    }
    if (!next) {
      result.message = `${machine.name} · 尚无可确认的兜底，请保存到草稿后编译`;
      return result;
    }
    at = next;
  }
  result.message = '没有入口节点';
  return result;
}
