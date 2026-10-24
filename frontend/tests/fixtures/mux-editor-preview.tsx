import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { DEFAULT_HOP_MUX, type ConsoleSnapshot, type Rule, type SnapshotStep } from '../../src/api';
import { RuleEditor, type ForwardPeer } from '../../src/panes/rules';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const peerName = params.has('longName') ? 'Tokyo-Singapore-long-relay-name-'.repeat(5) : '东京中继';
const initial: Rule[] = [
  {
    m: { t: 'any' },
    a: { t: 'forward', to: 'relay', dial: { t: 'overlay' }, pool: { t: 'mux', v: { ...DEFAULT_HOP_MUX } } },
  },
];
const source: SnapshotStep = { chain: 'chain', node: 'source', accept: null, hop_in: null, rules: initial };
const relay: SnapshotStep = {
  chain: 'chain',
  node: 'relay',
  accept: { uuid: 'preview-only', label: '中继' },
  hop_in: { port: 22000, security: { t: 'none' } },
  rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['snapshot'], {
  snapshot: {
    revision: 1,
    settings: { relay_mux: DEFAULT_HOP_MUX },
    apps: [
      {
        id: 'app',
        label: 'Mux 规则测试',
        chains: [{ id: 'chain', name: '测试线路' }],
        steps: [source, relay],
        fronts: [],
        grants: [],
        ingresses: [],
      },
    ],
    external_outbounds: [],
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot);
client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
client.setQueryData(['nodes'], {
  nodes: [
    { node_id: 'source', name: '香港入口', tenant_id: 'preview', egress_allowed: true },
    { node_id: 'relay', name: peerName, tenant_id: 'preview', egress_allowed: true },
  ],
});
const peers: ForwardPeer[] = [
  {
    id: 'relay',
    name: peerName,
    public_ipv4: '192.0.2.20',
    public_ipv6: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    step: relay,
    where: 'next',
    blocked: null,
  },
];
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <RuleEditor
      appId="app"
      chainId="chain"
      nodeId="source"
      initial={initial}
      accept={null}
      peers={peers}
      isForwardTarget={false}
      fallback={{ rules: [], pending: false }}
    />
  </QueryClientProvider>,
);
