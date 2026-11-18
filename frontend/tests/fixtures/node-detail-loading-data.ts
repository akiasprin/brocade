import { AGENT_PROTOCOL_VERSION, type NodeTrafficView, type NodeUnlockProbe } from '../../src/api';
import { UNLOCK_SERVICE_NAMES } from '../../src/panes/unlock-report';
import { NODES, SNAPSHOT, SETTINGS, COMPILED } from './rule-flow-data';
import { maintenanceRegions, maintenanceView } from './ip-maintenance-data';

export const nodes = NODES.map((node, index) => ({
  ...node,
  dns: { t: 'system' as const },
  domain_strategy: 'use_ip' as const,
  lifecycle_phase: 'active' as const,
  desired_poll_fresh: true,
  runtime_report_fresh: true,
  name: index === 0 ? '香港入口' : node.name,
  agent_protocol_version: AGENT_PROTOCOL_VERSION,
}));
export const nodeId = nodes[0].node_id;
export const snapshot = structuredClone(SNAPSHOT);
// Two real chains and the shared listener they reference, rather than a second rendering of the UI.
for (const app of snapshot.snapshot.apps) {
  app.chains = app.chains.filter((chain, index) => index < 2 || chain.id === 'shared');
  const ids = new Set(app.chains.map(chain => chain.id));
  app.steps = app.steps.filter(step => ids.has(step.chain));
  app.ingresses = app.ingresses.filter(ingress => ids.has(ingress.chain));
}
export const traffic: NodeTrafficView = {
  nodes: [
    {
      node_id: nodeId,
      tenant_id: nodes[0].tenant_id,
      name: nodes[0].name,
      cycle_kind: 'monthly',
      accounting_mode: 'bidirectional_sum',
      reset_month: null,
      reset_day: 1,
      quota_bytes: '1099511627776',
      period_start_unix_secs: 1793462400,
      period_end_unix_secs: 1796054400,
      rx_bytes: '1073741824',
      tx_bytes: '536870912',
      total_bytes: '1610612736',
      interface: 'eth0',
      tracking_started_at_unix_secs: 1793462400,
      last_reported_at_unix_secs: 1794888000,
      calibrated_at_unix_secs: null,
      last_gap_at_unix_secs: null,
      last_gap_reason: null,
      has_gap: false,
    },
  ],
};
const unlock: NodeUnlockProbe = {
  id: 'loading-fixture-probe',
  node_id: nodeId,
  family: 'v4',
  state: 'complete',
  created_at: 1794887900,
  completed_at: 1794887950,
  results: Object.keys(UNLOCK_SERVICE_NAMES).map(service => ({
    service,
    status: 'available',
    code: 'web_available',
    region: 'HK',
  })) as NodeUnlockProbe['results'],
};
export const cacheEntries: [string, readonly unknown[], unknown][] = [
  ['/nodes/agent-state', ['nodes'], { nodes }],
  ['/model/snapshot', ['snapshot'], snapshot],
  ['/settings', ['settings'], SETTINGS],
  ['/revisions?limit=50', ['revisions'], { current_revision: 1, revisions: [] }],
  [
    '/compile/1',
    ['compile', 1],
    {
      revision: 1,
      summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
      system: {},
      apps: COMPILED,
      diagnostics: [],
      redacted: false,
    },
  ],
  ['/node-traffic', ['node-traffic'], traffic],
  ['/certs', ['certs'], { groups: [], nodes: [], domain: null, sealing_available: true }],
];
export const responses: Record<string, unknown> = {
  ...Object.fromEntries(cacheEntries.map(([path, , value]) => [path, value])),
  '/probes/e2e?format=columnar-v1': { chains: [] },
  '/deployments?limit=50': { deployments: [] },
  '/agent-log-policy': { global: { agent_journal_mib: 100, xray_mib: 100, phantun_mib: 100 }, nodes: [] },
  '/ip-maintenance/regions': maintenanceRegions,
  [`/nodes/${nodeId}/ip-maintenance`]: maintenanceView(),
  [`/nodes/${nodeId}/unlock-probe/capabilities`]: { family_mode: 'split', ipv6_available: true, ipv6_reason: null },
  [`/nodes/${nodeId}/unlock-probe?family=v4`]: unlock,
  [`/nodes/${nodeId}/unlock-probe?family=v6`]: { ...unlock, family: 'v6' },
};
