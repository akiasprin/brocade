import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ChainRulesPanel } from '../../src/panes/chains';
import { RuleDraftScope, egressDnsSelectorKey } from '../../src/panes/rules';
import { PanelTitle } from '../../src/ui/icons';
import { draft } from '../../src/draft';
import { theme } from '../../src/forge/theme';
import { AGENT_NODES as BASE_NODES, APP, REVISIONS, SETTINGS, SNAPSHOT as BASE_SNAPSHOT } from './rule-layout-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
const SNAPSHOT = structuredClone(BASE_SNAPSHOT);
const AGENT_NODES = structuredClone(BASE_NODES);
const japan = {
  ...AGENT_NODES.nodes[2],
  node_id: 'jp-iij',
  name: '日本 IIJ',
  public_ipv4: '192.0.2.99',
  public_ipv6: '2001:db8:99::1',
  public_ipv4_country: 'JP',
};
AGENT_NODES.nodes.push(japan);
SNAPSHOT.snapshot.nodes.push({ id: japan.node_id, name: japan.name });
const app = SNAPSHOT.snapshot.apps[0];
if (params.has('shared'))
  app.steps[1].rules.splice(1, 0, { ...structuredClone(app.steps[1].rules[0]), m: { t: 'geosite', v: ['netflix'] } });
if (params.has('reverse')) {
  const rule = app.steps[1].rules[0];
  if (rule.a.t === 'forward') rule.a.dial = { t: 'reverse', v: 'v6' };
}
if (params.has('listener')) {
  app.chains.push({ ...app.chains[0], id: 'shared-listener', name: '共享出口' });
  app.steps.push({
    ...structuredClone(app.steps[2]),
    chain: 'shared-listener',
    hop_in: { port: 23000, security: { t: 'encryption' } },
  });
  app.steps[1].rules[0].a = {
    t: 'reuse_listener',
    listener: { chain: 'shared-listener', node: 'tw-hinet' },
    dial: { t: 'public', v: 'v4' },
    pool: { t: 'mux' },
  };
  app.ingresses.push({
    id: 'shared-entry',
    chain: 'shared-listener',
    node: 'tw-hinet',
    bind: '0.0.0.0',
    port: 25443,
    projection: {},
    guard: app.chains[0].guard,
    identity: app.chains[0].identity,
    wires: app.chains[0].wires,
  });
  app.steps = app.steps.filter(step => step.chain !== app.chains[0].id || step.node !== 'tw-hinet');
}
const STEPS = app.steps.filter(step => step.chain === app.chains[0].id);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
draft.init('rule-layout-acceptance');
if (params.has('container')) document.getElementById('root')!.style.maxWidth = `${Number(params.get('container'))}px`;
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['snapshot'], SNAPSHOT);
client.setQueryData(['nodes'], AGENT_NODES);
client.setQueryData(['settings'], SETTINGS);
client.setQueryData(['revisions'], REVISIONS);
globalThis.fetch = async (input, init) => {
  const path = String(input);
  if (path.endsWith('/model/preview')) {
    const next = structuredClone(SNAPSHOT);
    const ops = init?.body ? (JSON.parse(String(init.body)).ops as ReturnType<typeof draft.ops>) : draft.ops();
    for (const op of ops) {
      if (op.op === 'set_node_egress_dns') {
        const key = egressDnsSelectorKey(op.selector);
        const index = next.node_egress_dns.findIndex(
          policy => policy.node === op.node_id && egressDnsSelectorKey(policy.selector) === key,
        );
        if (index >= 0) next.node_egress_dns.splice(index, 1);
        if (op.resolution)
          next.node_egress_dns.push({
            node: op.node_id,
            position: index >= 0 ? index : next.node_egress_dns.length,
            selector: op.selector,
            resolution: op.resolution,
          });
      } else if (op.op === 'reorder_node_egress_dns') {
        for (const policy of next.node_egress_dns) {
          if (policy.node !== op.node_id) continue;
          policy.position = op.selectors.findIndex(
            selector => egressDnsSelectorKey(selector) === egressDnsSelectorKey(policy.selector),
          );
        }
      } else if (op.op === 'put_step') {
        const app = next.snapshot.apps.find(app => app.id === op.app_id);
        if (!app) throw new Error('Synthetic fixture has no app');
        const index = app.steps.findIndex(step => step.chain === op.chain_id && step.node === op.node_id);
        const step = { ...op.step, chain: op.chain_id, node: op.node_id };
        if (index < 0) app.steps.push(step);
        else app.steps[index] = { ...app.steps[index], ...step };
      } else if (op.op === 'prune_chain') {
        // These scenarios retain every node; no production pruning is simulated here.
      } else {
        return Response.json({ error: `合成夹具未提供该写入：${op.op}` }, { status: 400 });
      }
    }
    return Response.json({
      snapshot: next,
      compile: { diagnostics: [], summary: {}, system: { nodes: [] }, apps: [] },
      artifacts: { revision: 1, artifacts: [] },
    });
  }
  if (path.endsWith('/model/snapshot')) return Response.json(SNAPSHOT);
  if (path.endsWith('/nodes/agent-state')) return Response.json(AGENT_NODES);
  if (path.endsWith('/settings')) return Response.json(SETTINGS);
  if (path.endsWith('/revisions')) return Response.json(REVISIONS);
  return Response.json({ error: '合成夹具未提供该读取' }, { status: 404 });
};
Object.assign(window, { ruleLayoutDraftOps: () => draft.ops() });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <section className="chain-detail-section chain-detail-section-rules">
      <section className="panel config-panel rule-sheet-card node-chain-sheet">
        <header>
          <PanelTitle of="chains">链路规则</PanelTitle>
          <span className="rule-sheet-meta header-stat">1 跳</span>
        </header>
        <RuleDraftScope>
          <div className="node-chain-use chain-tree">
            <ChainRulesPanel
              appId={APP}
              chain={SNAPSHOT.snapshot.apps[0].chains[0]}
              spine={['hk-fxt', 'hk-akile']}
              steps={STEPS}
              nodes={AGENT_NODES.nodes}
              selected="hk-fxt"
              showHeader={false}
              onRemove={
                params.get('role') === 'readonly'
                  ? undefined
                  : node => {
                      throw new Error(`Removal requires a separate draft scenario: ${node}`);
                    }
              }
              readOnly={params.get('role') === 'readonly'}
            />
          </div>
        </RuleDraftScope>
      </section>
    </section>
  </QueryClientProvider>,
);
