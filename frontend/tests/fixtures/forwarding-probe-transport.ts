// In-memory API transport for production forwarding components; no target sockets.
import type { ForwardingOverview, ForwardingProbe, ForwardingTcpPingResult } from '../../src/api';
import { forwardingProbePending } from '../../src/panes/forwarding-probe';
import { endpointText, parseTarget } from '../../src/ui/forwarding-target';
export const PING_OUTCOMES = [
  { value: 'connected', label: '连通' },
  { value: 'connect-failed', label: '建连失败' },
  { value: 'dns-failed', label: 'DNS 失败' },
  { value: 'timeout', label: '连接超时' },
  { value: 'agent-upgrade-required', label: 'Agent 需升级' },
  { value: 'expired', label: '机器未返回' },
  { value: 'unavailable', label: '请求失败' },
] as const;
export type PingOutcome = (typeof PING_OUTCOMES)[number]['value'];

const path = '/users/ops/alice/forwarding';

/** Keep pending requests frozen to their saved rule and authorization, like the real endpoint. */
export function createMockProbeTransport(initial: ForwardingOverview, initialOutcome: PingOutcome) {
  let overview = initial;
  let outcome = initialOutcome;
  let sequence = 0;
  const records = new Map<
    number,
    { probe: ForwardingProbe; signature: string; started: number; outcome: Exclude<PingOutcome, 'unavailable'> }
  >();
  const snapshot = (ruleId: number) => {
    const rule = overview.rules.find(rule => rule.id === ruleId);
    const entry = overview.entitlements.find(entry => entry.node_id === rule?.node_id);
    // Usage increments and convergence alone do not invalidate a destination test.
    const signature = JSON.stringify([
      rule?.node_id,
      rule?.target_host,
      rule?.target_port,
      rule?.listen_port,
      rule?.protocol,
      rule?.name,
      rule?.enabled,
      rule?.updated_at,
      entry?.enabled,
      entry?.tcp_allowed,
      entry?.udp_allowed,
      entry?.listen_port_start,
      entry?.listen_port_end,
      entry?.max_rules,
      entry?.allow_private_targets,
      entry?.cycle_limit_bytes,
      entry?.quota_suspended,
    ]);
    return { rule, entry, signature };
  };
  const list = (): ForwardingProbe[] => {
    const result: ForwardingProbe[] = [];
    for (const [ruleId, record] of records) {
      const current = snapshot(ruleId);
      if (!current.rule) {
        records.delete(ruleId);
        continue;
      }
      if (record.signature !== current.signature) record.probe = { ...record.probe, state: 'obsolete', result: null };
      if (forwardingProbePending(record.probe)) {
        const elapsed = Date.now() - record.started;
        if (elapsed < 350) record.probe = { ...record.probe, state: 'queued' };
        else if (elapsed < 1200) record.probe = { ...record.probe, state: 'running' };
        else if (record.outcome === 'expired') record.probe = { ...record.probe, state: 'expired' };
        else {
          const target = parseTarget(endpointText(current.rule.target_host, current.rule.target_port));
          const domain = !('error' in target) && target.kind === '域名';
          const connected = record.outcome === 'connected';
          const reading: ForwardingTcpPingResult = {
            status: connected ? 'reachable' : record.outcome === 'agent-upgrade-required' ? 'not_run' : 'failed',
            code: record.outcome,
            connect_ms: connected ? 12 : null,
            dns_ms: domain && record.outcome !== 'agent-upgrade-required' ? 3 : null,
            resolved_ip:
              record.outcome === 'dns-failed' || record.outcome === 'agent-upgrade-required'
                ? null
                : domain
                  ? '192.0.2.80'
                  : current.rule.target_host.replace(/^\[|\]$/g, ''),
          };
          record.probe = { ...record.probe, state: 'complete', result: reading };
        }
      }
      result.push(record.probe);
    }
    return result;
  };
  return {
    list,
    update(next: ForwardingOverview, nextOutcome: PingOutcome) {
      overview = next;
      outcome = nextOutcome;
    },
    handle(input: RequestInfo | URL, init?: RequestInit): Response | null {
      const url = input instanceof Request ? input.url : String(input);
      if (url === `${path}/tcp-pings`) return Response.json(list());
      const match = url.match(/^\/users\/ops\/alice\/forwarding\/(\d+)\/tcp-ping$/);
      if (!match || init?.method !== 'POST') return null;
      if (outcome === 'unavailable') return Response.json({ error: '模拟请求失败，请重试' }, { status: 503 });
      const ruleId = Number(match[1]);
      const { rule, entry, signature } = snapshot(ruleId);
      if (!rule?.enabled || !entry?.enabled || entry.quota_suspended)
        return Response.json({ error: '规则当前不可测试' }, { status: 409 });
      const previous = list().find(probe => probe.rule_id === ruleId);
      if (previous && forwardingProbePending(previous)) return Response.json(previous);
      const started = Date.now();
      const probe: ForwardingProbe = {
        id: `mock-ping-${++sequence}`,
        rule_id: ruleId,
        rule_updated_at: rule.updated_at,
        generation: entry.desired_generation,
        state: 'queued',
        created_at: Math.floor(started / 1000),
        result: null,
      };
      records.set(ruleId, { probe, signature, started, outcome });
      return Response.json(probe);
    },
  };
}
