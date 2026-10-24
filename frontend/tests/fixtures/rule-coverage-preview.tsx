import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { DEFAULT_HOP_MUX, type ConsoleSnapshot, type Rule, type SnapshotStep } from '../../src/api';
import { RuleEditor, type ForwardPeer } from '../../src/panes/rules';
import { draft } from '../../src/draft';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
const listener = params.get('mode') === 'listener';
const readOnly = params.get('role') === 'readonly';
const mode = listener ? 'listener' : 'forward';
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
draft.init(`coverage-rule-${mode}`);
const name = params.has('longName') ? 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'.repeat(3) : '东京中继';
const baseRule: Rule = {
  m: { t: 'any' },
  a: listener
    ? {
        t: 'reuse_listener',
        listener: { chain: 'owner', node: 'relay' },
        dial: { t: 'overlay' },
        pool: { t: 'mux', v: { ...DEFAULT_HOP_MUX } },
      }
    : { t: 'forward', to: 'relay', dial: { t: 'overlay' }, pool: { t: 'mux', v: { ...DEFAULT_HOP_MUX } } },
};
const saved = draft
  .ops()
  .slice()
  .reverse()
  .find(op => op.op === 'put_step' && op.chain_id === 'chain' && op.node_id === 'source');
const rules = saved?.op === 'put_step' ? saved.step.rules : [baseRule];
const source: SnapshotStep = { chain: 'chain', node: 'source', accept: null, hop_in: null, rules };
const relay: SnapshotStep = {
  chain: listener ? 'owner' : 'chain',
  node: 'relay',
  accept: { uuid: 'preview-only', label: '中继' },
  hop_in: { port: 22000, security: { t: 'none' } },
  rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
};
const chain = (id: string, label: string) => ({
  id,
  name: label,
  tenant: 'preview',
  revision: 1,
  overlay: { ipv4: true, ipv6: false },
  auto_exits: [],
  egress_via: [],
  carry_via: [],
  mtu: 1420,
  hop_mode: 'direct',
  hop_port: 22000,
  projection: {},
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
    cn_source_block: 'off',
  },
  identity: { public_key: 'preview', short_ids: ['0123abcd'] },
  wires: { vless: { kind: 'vless-reality' } },
});
const model = {
  snapshot: {
    revision: 1,
    settings: { relay_mux: DEFAULT_HOP_MUX },
    external_outbounds: [],
    apps: [
      {
        id: 'app',
        label: '源项目',
        chains: [chain('chain', '源线路')],
        steps: listener ? [source] : [source, relay],
        fronts: [],
        grants: [],
        ingresses: [],
      },
      ...(listener
        ? [
            {
              id: 'owner-app',
              label: '监听项目',
              chains: [chain('owner', '共享出口')],
              steps: [relay],
              fronts: [],
              grants: [],
              ingresses: [
                {
                  ...chain('owner', '共享出口'),
                  id: 'owner-in',
                  chain: 'owner',
                  node: 'relay',
                  bind: '0.0.0.0',
                  port: 443,
                },
              ],
            },
          ]
        : []),
    ],
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;
const nodes = {
  nodes: [
    { node_id: 'source', name: '香港入口', tenant_id: 'preview', egress_allowed: true, retired_at: null },
    {
      node_id: 'relay',
      name,
      tenant_id: 'preview',
      egress_allowed: true,
      retired_at: null,
      public_ipv4: '192.0.2.20',
      public_ipv6: null,
      public_ipv4_nat: false,
      public_ipv6_nat: false,
    },
  ],
};
const settings = { ports: { hop_base: 20000 } };
const revisions = { current_revision: null, revisions: [] };
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['snapshot'], model);
client.setQueryData(['nodes'], nodes);
client.setQueryData(['settings'], settings);
client.setQueryData(['revisions'], revisions);
globalThis.fetch = async (input, init) => {
  const path = String(input);
  if (path.endsWith('/model/preview')) {
    const next = structuredClone(model);
    const ops = init?.body ? JSON.parse(String(init.body)).ops : draft.ops();
    for (const op of ops ?? [])
      if (op.op === 'put_step') {
        const app = next.snapshot.apps.find(a => a.id === op.app_id);
        if (!app) continue;
        const index = app.steps.findIndex(s => s.chain === op.chain_id && s.node === op.node_id);
        const step = { ...op.step, chain: op.chain_id, node: op.node_id };
        if (index < 0) app.steps.push(step);
        else app.steps[index] = step;
      }
    return Response.json({
      snapshot: next,
      compile: { diagnostics: [], summary: {}, system: { nodes: [] }, apps: [] },
      artifacts: { revision: 1, artifacts: [] },
    });
  }
  if (path.endsWith('/model/snapshot')) return Response.json(model);
  if (path.endsWith('/nodes/agent-state')) return Response.json(nodes);
  if (path.endsWith('/settings')) return Response.json(settings);
  if (path.endsWith('/revisions')) return Response.json(revisions);
  return Response.json({ error: '合成夹具未提供该读取' }, { status: 404 });
};
Object.assign(window, { coverageDraftOps: () => draft.ops() });
const peers: ForwardPeer[] = listener
  ? []
  : [
      {
        id: 'relay',
        name,
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
    <section className="panel config-panel rule-sheet-card node-chain-sheet">
      <div className="chain-rule-tree">
        <div className="chain-rule-body">
          <RuleEditor
            appId="app"
            chainId="chain"
            nodeId="source"
            initial={rules}
            accept={null}
            peers={peers}
            isForwardTarget={false}
            fallback={{ rules: [], pending: false }}
            readOnly={readOnly}
          />
        </div>
      </div>
    </section>
  </QueryClientProvider>,
);
