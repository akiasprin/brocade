// Production components on synthetic local data. No request reaches the Console or a destination.
import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ForwardingEntitlement, ForwardingRule, UserListItem, UserTrafficAccountingMode } from '../../src/api';
import { UserForwardingCard } from '../../src/panes/user-forwarding';
import { theme } from '../../src/forge/theme';
import { jobsOf, cycleUsageOf, overviewOf, SCENES, type SceneKey } from './forwarding-actions-mockup-data';
import { createMockProbeTransport } from './forwarding-actions-mockup-probe';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'dark' ? 'dark' : 'light')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
const sceneKey = params.get('scene') ?? 'typical';
const scene = SCENES[Object.hasOwn(SCENES, sceneKey) ? (sceneKey as SceneKey) : 'typical']();
let current = overviewOf(scene);
const nodeOptions = [...scene.entries, ...scene.candidates].map(node => ({
  id: node.id,
  name: node.name,
  api_port: 7443,
}));
const probes = createMockProbeTransport(current, 'connected');
let failed = false;
const rulesFor = (entry: ForwardingEntitlement) => {
  current.rules = current.rules.map(rule =>
    rule.node_id !== entry.node_id
      ? rule
      : {
          ...rule,
          runtime_status:
            !entry.enabled || !rule.enabled ? 'disabled' : entry.quota_suspended ? 'quota-paused' : 'active',
        },
  );
};
const response = () => {
  current.entitlements = current.entitlements.map(entry => ({
    ...entry,
    rule_count: current.rules.filter(rule => rule.node_id === entry.node_id).length,
  }));
  return Response.json(current);
};
globalThis.fetch = async (input, init) => {
  const path = String(input);
  probes.update(current, 'connected');
  const probeResponse = probes.handle(input, init);
  if (probeResponse) return probeResponse;
  if (path === '/nodes/agent-state')
    return Response.json({
      nodes: [...scene.entries, ...scene.candidates].map(node => ({
        node_id: node.id,
        public_ipv4_country: node.country,
      })),
    });
  if (path === '/usage/user-cycles') return Response.json(cycleUsageOf(scene));
  const jobs = path.match(/forwarding-nodes\/([^/]+)\/quota\/jobs/);
  if (jobs) return Response.json(jobsOf(scene, decodeURIComponent(jobs[1])));
  if (init?.method && init.method !== 'GET') {
    await new Promise(resolve => setTimeout(resolve, 200));
    const node = path.match(/\/forwarding-nodes\/([^/]+)(\/quota)?$/);
    if (node) {
      const id = decodeURIComponent(node[1]);
      const existing = current.entitlements.find(entry => entry.node_id === id);
      if (init.method === 'DELETE') {
        current.entitlements = current.entitlements.filter(entry => entry.node_id !== id);
        current.rules = current.rules.filter(rule => rule.node_id !== id);
        return response();
      }
      if (!failed && params.get('fail') === (node[2] ? 'quota-once' : 'policy-once')) {
        failed = true;
        return Response.json({ error: '模拟保存失败，请重试' }, { status: 503 });
      }
      const body = JSON.parse(String(init.body));
      if (node[2] && existing) {
        existing.cycle_limit_bytes = body.limit_bytes;
        existing.quota_suspended = body.limit_bytes != null && (existing.cycle_used_bytes ?? 0) >= body.limit_bytes;
        rulesFor(existing);
      } else if (existing) {
        Object.assign(existing, body);
        existing.desired_generation += 1;
        existing.applied_generation = existing.desired_generation;
        existing.reconcile_status = 'converged';
        rulesFor(existing);
      } else {
        const option = nodeOptions.find(node => node.id === id);
        current.entitlements.push({
          ...body,
          node_id: id,
          node_name: option?.name ?? id,
          public_ipv4: '192.0.2.200',
          public_ipv6: null,
          rule_count: 0,
          desired_generation: 1,
          applied_generation: 1,
          reconcile_status: 'converged',
          last_error: null,
          cycle_limit_bytes: null,
          cycle_used_bytes: 0,
        });
      }
      return response();
    }
    const ruleId = path.match(/\/forwarding\/(\d+)$/);
    if (ruleId && init.method === 'DELETE') {
      current.rules = current.rules.filter(rule => rule.id !== Number(ruleId[1]));
      return response();
    }
    const body = JSON.parse(String(init.body));
    if (ruleId)
      current.rules = current.rules.map(rule =>
        rule.id === Number(ruleId[1]) ? { ...rule, ...body, updated_at: new Date().toISOString() } : rule,
      );
    else if (path.endsWith('/forwarding') && init.method === 'POST') {
      current.rules.push({
        ...body,
        id: Math.max(0, ...current.rules.map(rule => rule.id)) + 1,
        tenant_id: current.tenant_id,
        user_id: current.user_id,
        runtime_status: 'active',
        uplink_bytes: 0,
        downlink_bytes: 0,
        created_at: new Date().toISOString(),
        updated_at: new Date().toISOString(),
      } as ForwardingRule);
    }
    const entry = current.entitlements.find(entry => entry.node_id === body.node_id);
    if (entry) rulesFor(entry);
    return response();
  }
  if (path.endsWith('/forwarding')) return response();
  return Response.json({ error: 'Isolated fixture: unhandled request' }, { status: 404 });
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
function Preview() {
  const [mode, setMode] = useState<UserTrafficAccountingMode>('single');
  return (
    <QueryClientProvider client={client}>
      <div className="user-dbody">
        <UserForwardingCard
          user={{ tenant_id: 'ops', id: 'alice', traffic_accounting_mode: mode } as UserListItem}
          nodes={nodeOptions}
          canAuthorize={params.get('readonly') !== '1'}
          canManageRules={params.get('readonly') !== '1'}
          accountingDisabled={false}
          onAccountingModeChange={setMode}
        />
      </div>
    </QueryClientProvider>
  );
}
createRoot(document.getElementById('root')!).render(<Preview />);
