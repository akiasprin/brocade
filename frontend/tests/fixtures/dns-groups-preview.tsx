// Synthetic data; production machine DNS editor and path details, with no real API requests.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { MachineEgressDnsRules } from '../../src/panes/rules';
import { UnlockDnsDetails } from '../../src/panes/unlock-dns';
import { unlockReport } from '../../src/panes/unlock-report';
import { theme } from '../../src/forge/theme';
import type { ConsoleSnapshot, EgressDnsResolution, NodeUnlockProbe } from '../../src/api';
import '../../src/styles.css';
import '../../src/panes/node-unlock.css';

const params = new URLSearchParams(location.search);
const selectedTheme = params.get('theme') === 'dark' ? 'dark' : 'light';
const editing = params.get('scene') === 'editing';
if (theme.snapshot() !== selectedTheme) theme.toggle();
const selector = { t: 'geosite' as const, v: ['category-ai-!cn'] };
const resolution: EgressDnsResolution = {
  address: params.get('scene') === 'legacy' ? '192.0.2.53' : '192.0.2.53, 198.51.100.53, 2001:db8::53',
  port: 53,
  transport: 'udp',
  address_strategy: 'use_ip',
  fallback: 'machine',
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
const snapshot: ConsoleSnapshot = {
  snapshot: {
    revision: 1,
    apps: [],
    external_outbounds: [],
    nodes: [
      { id: 'preview-exit', name: '香港落地' },
      { id: 'preview-source', name: '新加坡落地' },
    ],
  },
  node_egress_dns: [
    { node: 'preview-exit', position: 0, selector, resolution },
    ...(editing
      ? [
          { node: 'preview-exit', position: 1, selector: { t: 'geosite' as const, v: ['disney'] }, resolution },
          {
            node: 'preview-exit',
            position: 2,
            selector: { t: 'geosite' as const, v: ['google'] },
            resolution: { ...resolution, address: '1.1.1.1', address_strategy: 'use_ipv6' as const },
          },
          { node: 'preview-source', position: 1, selector, resolution },
          {
            node: 'preview-source',
            position: 0,
            selector: { t: 'geosite' as const, v: ['netflix'] },
            resolution: { ...resolution, port: 5353, transport: 'tcp' as const, fallback: 'stop' as const },
          },
        ]
      : []),
  ],
  redacted: false,
};
client.setQueryData(['snapshot'], snapshot);
const grouped = { ...resolution, address: ['192.0.2.53', '198.51.100.53', '2001:db8::53'] };
const job: NodeUnlockProbe = {
  id: 'preview',
  node_id: 'preview-exit',
  state: 'complete',
  family: 'v6',
  created_at: 1,
  completed_at: 2,
  results: [
    {
      service: 'chatgpt',
      status: 'available',
      code: 'web_available',
      region: 'SG',
      dns_parts: [
        {
          part: 'service',
          kind: 'unknown',
          exits: ['preview-exit'],
          paths: [
            {
              exit: 'preview-exit',
              normal_family: null,
              fallback: 'machine',
              answer_from: 'policy',
              reason: 'resolver_error',
              rules: [
                {
                  selector,
                  resolution: { ...resolution, address: '203.0.113.53' },
                  members: [{ index: 1, outcome: 'error', other: false, attempts: 5, elapsed_ms: 4000, cache: 'miss' }],
                },
                {
                  selector,
                  resolution: grouped,
                  selected_member: 2,
                  members: [
                    {
                      index: 1,
                      outcome: 'error',
                      other: false,
                      attempts: 1,
                      elapsed_ms: 12,
                      rcode: 5,
                      cache: 'miss',
                    },
                    { index: 2, outcome: 'answer', other: false, attempts: 2, elapsed_ms: 280, cache: 'miss' },
                  ],
                },
              ],
            },
          ],
        },
      ],
    },
  ],
};
const row = unlockReport(job).rows[0];
globalThis.fetch = async () => {
  throw new Error('Unexpected DNS preview request');
};
const preview = (
  <main
    className="cardpage"
    style={{
      maxWidth: editing || params.get('scene') === 'create' ? 1760 : 1180,
      margin: '24px auto',
      padding: '0 16px',
    }}
  >
    {editing && <h1 style={{ marginBottom: 20 }}>香港落地 · 规则</h1>}
    <section
      className={
        editing ? 'panel config-panel rule-sheet-card node-egress-rule-sheet' : 'cg-sec node-egress-rule-sheet'
      }
    >
      {!editing && (
        <header className="cg-sec-head">
          <h2>香港落地 · DNS 规则</h2>
        </header>
      )}
      <MachineEgressDnsRules
        nodeId="preview-exit"
        nodeName="香港落地"
        showHeader={editing || params.get('scene') === 'create'}
      />
    </section>
    {!editing && (
      <section className="cg-sec" style={{ marginTop: 20, padding: 16 }}>
        <h3>ChatGPT · IPv6 解析路径</h3>
        <UnlockDnsDetails row={row} />
      </section>
    )}
  </main>
);
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    {editing ? (
      <div data-dns-preview-scroll style={{ height: '100dvh', overflowY: 'auto' }}>
        {preview}
      </div>
    ) : (
      preview
    )}
  </QueryClientProvider>,
);
