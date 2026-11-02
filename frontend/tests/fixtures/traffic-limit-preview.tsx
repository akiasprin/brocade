// Exercise actual receipt handling in the production TrafficAccountingCard.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { NodeAgentStateItem, NodeTrafficItem, TrafficLimitObservation } from '../../src/api';
import { TrafficAccountingCard } from '../../src/panes/nodes';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';
const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const GiB = 1024 ** 3;
const now = Math.floor(Date.now() / 1000);
const start = Date.UTC(2026, 9, 1) / 1000;
const layer = (phase: 'limited' | 'waiting' | 'error', rate_bps: number | null, error: string | null = null) => ({
  phase,
  rate_bps,
  error,
});
const scenes: Record<string, Partial<TrafficLimitObservation>> = {
  handoff: {
    phase: 'waiting',
    system: layer('limited', 100_000_000),
    application: layer('waiting', null, '等待旧代排空后再确认应用层上限'),
  },
  heartbeat: { phase: 'limited', application: layer('limited', 100_000_000) },
  partial: {
    phase: 'error',
    error: 'RTNETLINK answers: Operation not permitted',
    system: layer('error', null, 'RTNETLINK answers: Operation not permitted'),
    application: layer('limited', 100_000_000),
  },
};
const scene = params.get('scene') ?? 'handoff';
const rx = (scene === 'partial' ? 1044 : 1200) * GiB;
const tx = (scene === 'partial' ? 702 : 600) * GiB;
const item: NodeTrafficItem = {
  node_id: 'hk-01',
  tenant_id: 'platform',
  name: '香港示例 01',
  cycle_kind: 'monthly',
  accounting_mode: 'bidirectional_sum',
  reset_month: null,
  reset_day: 1,
  quota_bytes: String(2 * 1024 ** 4),
  rx_bytes: String(rx),
  tx_bytes: String(tx),
  total_bytes: String(rx + tx),
  period_start_unix_secs: start,
  period_end_unix_secs: Date.UTC(2026, 10, 1) / 1000,
  interface: 'eth0',
  tracking_started_at_unix_secs: Date.UTC(2026, 8, 3) / 1000,
  last_reported_at_unix_secs: now - 20,
  calibrated_at_unix_secs: null,
  last_gap_at_unix_secs: null,
  last_gap_reason: null,
  has_gap: false,
  limit: {
    revision: 3,
    supported: true,
    policy: {
      enabled: true,
      layer: scene === 'heartbeat' ? 'application' : 'both',
      rules: [
        { threshold: { kind: 'quota_percent', basis_points: 8000 }, rate_bps: 100_000_000 },
        { threshold: { kind: 'quota_percent', basis_points: 9500 }, rate_bps: 10_000_000 },
      ],
    },
    observation: {
      revision: 3,
      phase: 'limited',
      interface: 'eth0',
      rate_bps: 100_000_000,
      used_bytes: String(rx + tx),
      period_start_unix_secs: start,
      error: null,
      system: null,
      application: null,
      ...scenes[scene],
    },
    reported_at_unix_secs: now - (scene === 'heartbeat' ? 240 : 40),
  },
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['node-traffic'], { nodes: [item] });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <div className="nd-tab-config">
      <div>
        <TrafficAccountingCard node={{ node_id: item.node_id, name: item.name } as NodeAgentStateItem} canEdit />
      </div>
    </div>
  </QueryClientProvider>,
);
