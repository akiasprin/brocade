// Synthetic history across a group move; uses the production card and source-detail dialog.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ConsoleSnapshot, NodeInflowRow, NodeInflowUsage } from '../../src/api';
import { NodeInflowUsageCard, trafficSources } from '../../src/node-traffic';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';

if (theme.snapshot() !== (new URLSearchParams(location.search).get('theme') === 'light' ? 'light' : 'dark'))
  theme.toggle();
document.documentElement.dataset.palette = 'dailan';
const snapshot = {
  redacted: false,
  node_egress_dns: [],
  snapshot: {
    revision: 1,
    apps: [
      {
        id: 'new',
        label: '新分组',
        chains: [
          { id: 'hk', name: '香港 01 @ FxT', tenant: 'platform', subscription_country: 'HK' },
          { id: 'jp', name: '日本 02 IIJ', tenant: 'platform', subscription_country: 'JP' },
        ],
        ingresses: [],
        grants: [],
        fronts: [],
        steps: [],
      },
    ],
  },
} satisfies ConsoleSnapshot;
const row = (user: string, app: string, chain: string, amount: number): NodeInflowRow => ({
  tenant_id: 'platform',
  user_id: user,
  source: 'chain',
  app_id: app,
  chain_id: chain,
  ingress_id: null,
  downlink_bytes: Math.round(amount * 1024 ** 3),
  uplink_bytes: 0,
  has_gap: false,
});
const data: NodeInflowUsage = {
  period: 'month',
  range_start_unix_secs: 1790784000,
  range_end_unix_secs: 1793269200,
  period_end_unix_secs: 1793462400,
  latest_report_unix_secs: 1793269200,
  idle_users: [],
  range_limited: false,
  rows: [
    row('alice', 'old', 'hk', 4.2),
    row('alice', 'new', 'hk', 1.6),
    row('alice', 'new', 'jp', 0.8),
    row('bob', 'old', 'hk', 1.3),
    row('bob', 'new', 'hk', 1.1),
    row('bob', 'old', 'deleted', 0.6),
  ],
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
client.setQueryData(['node-inflow-usage', 'n1', 'month'], data);
window.fetch = async () =>
  new Response(
    JSON.stringify({
      ...data,
      observed_since_unix_secs: data.range_start_unix_secs,
      limited: false,
      accounting_mismatch: false,
      accounting_uplink_bytes: 0,
      accounting_downlink_bytes: data.rows
        .filter(row => row.user_id === 'alice')
        .reduce((sum, row) => sum + row.downlink_bytes, 0),
      rows: data.rows.filter(row => row.user_id === 'alice').map(row => ({ ...row, source_ip: '192.0.2.1' })),
    }),
    { status: 200, headers: { 'Content-Type': 'application/json' } },
  );
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <main style={{ padding: 16 }}>
      <NodeInflowUsageCard nodeId="n1" period="month" sourceOf={trafficSources(snapshot)} />
    </main>
  </QueryClientProvider>,
);
