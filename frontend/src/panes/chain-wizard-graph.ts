import type { HopDial, Rule } from '../api';
import { defaultHopWire, formatHostPort, hostOf } from './rules';

export type WizardRuleTables = Record<string, Rule[]>;

export const wizardEgressRule = (): Rule => ({
  m: { t: 'any' },
  a: { t: 'egress', send_through: null },
});

/** The main path is a view of the Any-forward rules, never another saved topology. */
export function wizardSpine(root: string | null, tables: WizardRuleTables): string[] {
  if (!root) return [];
  const path = [root];
  const seen = new Set(path);
  while (true) {
    const current = path[path.length - 1];
    const tail = tables[current]?.find(rule => rule.m.t === 'any');
    const next = tail?.a.t === 'forward' ? tail.a.to : null;
    if (!next || seen.has(next)) return path;
    path.push(next);
    seen.add(next);
  }
}

/** Every Forward edge, including exceptions, contributes a member of the new chain. */
export function wizardMembers(root: string | null, tables: WizardRuleTables): string[] {
  if (!root) return [];
  const seen = new Set([root]);
  const queue = [root];
  for (let i = 0; i < queue.length; i += 1) {
    for (const rule of tables[queue[i]] ?? []) {
      if (rule.a.t !== 'forward' || !rule.a.to || seen.has(rule.a.to)) continue;
      seen.add(rule.a.to);
      queue.push(rule.a.to);
    }
  }
  return queue;
}

/** A removed edge drops only nodes no longer reachable by any other rule. */
export function reachableWizardTables(root: string | null, tables: WizardRuleTables): WizardRuleTables {
  const members = wizardMembers(root, tables);
  return Object.fromEntries(members.map(id => [id, tables[id] ?? [wizardEgressRule()]]));
}

export interface WizardForwardEdge {
  source: string;
  target: string;
  listener: string;
  dial: HopDial;
}

/** Reverse edges listen on their source; all other Forward edges listen on their target. */
export function wizardForwardEdges(root: string | null, tables: WizardRuleTables): WizardForwardEdge[] {
  return wizardMembers(root, tables).flatMap(source =>
    (tables[source] ?? []).flatMap(rule =>
      rule.a.t === 'forward' && rule.a.to
        ? [
            {
              source,
              target: rule.a.to,
              listener: rule.a.dial.t === 'reverse' ? source : rule.a.to,
              dial: rule.a.dial,
            },
          ]
        : [],
    ),
  );
}

export function wizardDefaultListenerWire(edges: WizardForwardEdge[], host: string): 'none' | 'encryption' {
  return edges.some(edge => edge.listener === host && defaultHopWire(edge.dial) === 'encryption')
    ? 'encryption'
    : 'none';
}

/** Port belongs to the listener Step; address dials must follow edits to that single port. */
export function wizardRulesWithListenerPorts(
  root: string | null,
  tables: WizardRuleTables,
  portOf: (host: string) => number,
): WizardRuleTables {
  return Object.fromEntries(
    wizardMembers(root, tables).map(source => [
      source,
      (tables[source] ?? []).map(rule => {
        if (rule.a.t !== 'forward' || rule.a.dial.t !== 'addr') return rule;
        const listener = rule.a.to;
        return {
          ...rule,
          a: {
            ...rule.a,
            dial: { t: 'addr' as const, v: formatHostPort(hostOf(rule.a.dial), portOf(listener)) },
          },
        };
      }),
    ]),
  );
}

/** Keep the rule table's terminal and target invariants before staging any put_step. */
export function wizardRuleIssue(root: string | null, tables: WizardRuleTables): string | null {
  if (!root) return '选择入口节点';
  for (const node of wizardMembers(root, tables)) {
    const rules = tables[node] ?? [];
    if (rules.filter(rule => rule.m.t === 'any').length !== 1 || rules.at(-1)?.m.t !== 'any')
      return `${node} 需要一条位于末尾的「任意」规则`;
    if (rules.filter(rule => rule.m.t === 'sniffing_failed').length > 1) return `${node} 只能有一条嗅探失败兜底规则`;
    for (const rule of rules) {
      if ('v' in rule.m && Array.isArray(rule.m.v) && rule.m.v.length === 0)
        return `${node} 的${rule.m.t}匹配值不能为空`;
      if (rule.m.t === 'domain_regex' && !rule.m.v.trim()) return `${node} 的域名正则不能为空`;
      if (rule.a.t === 'forward' && !rule.a.to) return `${node} 的转发目标不能为空`;
      if (rule.a.t === 'proxy' && !rule.a.outbound) return `${node} 的代理出站不能为空`;
    }
  }
  const visiting = new Set<string>();
  const visited = new Set<string>();
  const visit = (node: string): boolean => {
    if (visiting.has(node)) return true;
    if (visited.has(node)) return false;
    visiting.add(node);
    for (const rule of tables[node] ?? []) {
      if (rule.a.t === 'forward' && rule.a.to && visit(rule.a.to)) return true;
    }
    visiting.delete(node);
    visited.add(node);
    return false;
  };
  return visit(root) ? '转发规则不能成环' : null;
}
