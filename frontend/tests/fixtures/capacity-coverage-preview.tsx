import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { MachineTrafficModelNode, MachineTrafficModelView } from '../../src/api';
import { CapacityPane } from '../../src/panes/capacity';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';
const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const day = 86400,
  start = Date.UTC(2026, 9, 1) / 1000,
  now = start + 23.5 * day,
  end = start + 31 * day;
const gib = (n: number) => String(n * 1024 ** 3);
const node: MachineTrafficModelNode = {
  node_id: 'coverage-over',
  tenant_id: 'preview',
  name: '跨区域高流量机器',
  cycle_kind: 'monthly',
  accounting_mode: 'bidirectional_sum',
  reset_month: null,
  reset_day: 1,
  period_start_unix_secs: start,
  period_end_unix_secs: end,
  previous_period_start_unix_secs: start - 30 * day,
  quota_bytes: gib(240),
  used_bytes: gib(18000),
  previous_period_bytes: '0',
  daily_rate_bytes: gib(800),
  rate_days: 28,
  projected_p50_bytes: gib(27000),
  projected_p90_bytes: gib(32000),
  band: { basis: 'node', horizon_days: 8, samples: 120, upper_error_basis_points: 1800 },
  confidence: 'high',
  needs_calibration: false,
  days: Array.from({ length: 24 }, (_, i) => ({ day_start_unix_secs: start + i * day, bytes: gib(750) })),
  previous_days: [],
  tracking_started_at_unix_secs: start - 90 * day,
  calibrated_at_unix_secs: null,
  gap_at_unix_secs: null,
  gap_reason: null,
  last_period_accuracy: null,
  users: [],
  unattributed_bytes: gib(18000),
  projected_unattributed_bytes: gib(27000),
};
const model: MachineTrafficModelView = {
  generated_at_unix_secs: now,
  attribution: 'current_topology_proportional',
  nodes: [node],
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['usage-machine-model'], model);
client.setQueryData(['nodes'], { nodes: [{ node_id: node.node_id, name: node.name, public_ipv4_country: 'JP' }] });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <CapacityPane />
  </QueryClientProvider>,
);
