// Visual acceptance fixture: the production 流入明细 dialog with synthetic sources only; no network access.
// Addresses come from the documentation ranges (RFC 5737, RFC 3849).
// ?scene=typical|single|notice|masked|empty chooses the response, ?theme=dark|light the theme.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { NetworkOperator, NodeInflowDetailRow, NodeInflowDetails } from '../../src/api';
import { NodeInflowDetailsDialog } from '../../src/node-inflow-details';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
const scene = params.get('scene') ?? 'typical';

const GiB = 1024 ** 3;
const NOW = Date.UTC(2026, 9, 18, 13, 40) / 1000;
const DAY_START = Date.UTC(2026, 9, 17, 16) / 1000;
const CHAINS: Record<string, { name: string; country: string; color: string }> = {
  jp: { name: '日本 02 IIJ', country: 'JP', color: 'var(--observe-1)' },
  hk: { name: '香港 01 直出', country: 'HK', color: 'var(--observe-6)' },
};
const row = (
  chain: string | null,
  ip: string | null,
  down: number,
  up: number,
  source: NodeInflowDetailRow['source'] = 'chain',
): NodeInflowDetailRow => ({
  tenant_id: 'preview',
  user_id: 'alice',
  source,
  app_id: source === 'chain' ? 'apac' : null,
  chain_id: source === 'chain' ? chain : null,
  ingress_id: null,
  source_ip: ip,
  downlink_bytes: Math.round(down * GiB),
  uplink_bytes: Math.round(up * GiB),
  has_gap: false,
});
const PHONE = '2001:db8:4c1a:4a29:9017:1598:ffdf:c0ba';
const LAPTOP = '2001:db8:91f0:2c07:5a1e:77b4:e02d:13a9';
const typical = [
  row('jp', PHONE, 29.4, 1.29),
  row('jp', '192.0.2.95', 17.6, 0.64),
  row('jp', '203.0.113.24', 2.1, 0.16),
  row('jp', '198.51.100.221', 0.37, 0.02),
  row('jp', null, 0.3, 0.01),
  row('hk', '203.0.113.24', 3.2, 0.2),
  row('hk', LAPTOP, 2.4, 0.3),
  row('hk', '198.51.100.7', 1.1, 0.05),
  row('hk', '192.0.2.41', 0.82, 0.04),
  row('hk', '198.51.100.38', 0.41, 0.02),
  row('hk', '203.0.113.180', 0.33, 0.01),
  row('hk', '192.0.2.200', 0.21, 0.01),
  row('hk', '2001:db8:91f0:2c07:1d4e:9a02:6b3c:f81e', 0.12, 0.01),
  row('hk', '203.0.113.9', 0.05, 0),
  row(null, null, 1.6, 0.3, 'forwarding'),
];
const located: [string, string, NetworkOperator?][] = [
  [PHONE, 'CN', 'cmcc'],
  ['192.0.2.95', 'JP'],
  ['203.0.113.24', 'CN', 'chinanet'],
  ['198.51.100.221', 'CN', 'unicom'],
  [LAPTOP, 'US'],
  ['192.0.2.41', 'CN', 'cernet'],
  ['198.51.100.38', 'SG'],
  ['203.0.113.180', 'CN', 'chinanet'],
  ['192.0.2.200', 'HK', 'cmcc'],
  ['2001:db8:91f0:2c07:1d4e:9a02:6b3c:f81e', 'US'],
  ['203.0.113.9', 'CN', 'unicom'],
];
const rowsFor: Record<string, NodeInflowDetailRow[]> = {
  typical,
  single: typical.filter(entry => entry.chain_id === 'jp'),
  // Mismatched accounting: the server merges what it cannot attribute into one unattributed row.
  notice: [
    { ...typical[0], has_gap: true },
    ...typical.slice(1, 4),
    ...typical.slice(5, 9),
    row(null, null, 0.9, 0.08, 'unattributed'),
  ],
  // Read-only operators receive addresses masked at the Console boundary; equal masks may be different devices.
  masked: [
    row('jp', '2001:db8:***', 29.4, 1.29),
    row('jp', '192.0.***.***', 17.6, 0.64),
    row('jp', '203.0.***.***', 2.1, 0.16),
    row('hk', '203.0.***.***', 3.2, 0.2),
    row('hk', '2001:db8:***', 2.4, 0.3),
    row('hk', '198.51.***.***', 1.1, 0.05),
  ],
  empty: [],
};
const rows = rowsFor[scene] ?? typical;
const masked = scene === 'masked';
const response: NodeInflowDetails = {
  range_start_unix_secs: DAY_START,
  range_end_unix_secs: NOW,
  observed_since_unix_secs: DAY_START,
  latest_report_unix_secs: NOW,
  limited: scene === 'notice',
  accounting_mismatch: scene === 'notice',
  accounting_uplink_bytes: Math.round(2.71 * GiB),
  accounting_downlink_bytes: Math.round(57.94 * GiB),
  rows,
  source_countries: masked
    ? [
        { ip: '2001:db8:***', country: 'CN' },
        { ip: '2001:db8:***', country: 'US' },
        { ip: '192.0.***.***', country: 'JP' },
        { ip: '203.0.***.***', country: 'CN' },
        { ip: '198.51.***.***', country: 'CN' },
      ]
    : located.map(([ip, country]) => ({ ip, country })),
  source_operators: masked
    ? [{ ip: '198.51.***.***', operator: 'unicom' }]
    : located.flatMap(([ip, , operator]) => (operator ? [{ ip, operator }] : [])),
};

globalThis.fetch = async input => {
  if (String(input).includes('/inflow-details')) return Response.json(response);
  return Response.json({ error: 'not part of this preview' }, { status: 404 });
};

const chainOf = (key: string) => (key.startsWith('[') ? CHAINS[(JSON.parse(key) as string[])[1]] : undefined);
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <NodeInflowDetailsDialog
      nodeId="n1"
      nodeName="香港测试机器"
      tenantId="preview"
      userId="alice"
      period="day"
      sourceOf={key => ({ key, name: chainOf(key)?.name ?? key, country: chainOf(key)?.country ?? null })}
      colorOf={key => chainOf(key)?.color}
      onClose={() => {}}
    />
  </QueryClientProvider>,
);
