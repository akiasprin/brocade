import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ChainRulesPanel } from '../../src/panes/chains';
import { RuleDraftScope, egressDnsSelectorKey } from '../../src/panes/rules';
import { PanelTitle } from '../../src/ui/icons';
import { draft } from '../../src/draft';
import { theme } from '../../src/forge/theme';
import { AGENT_NODES, APP, REVISIONS, SETTINGS, SNAPSHOT, STEPS } from './rule-layout-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
draft.init('rule-layout-acceptance');
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
        <RuleDraftScope hint="改动落进草稿，顶栏按「提交」才写进库。">
          <div className="node-chain-use chain-tree">
            <ChainRulesPanel
              appId={APP}
              chain={SNAPSHOT.snapshot.apps[0].chains[0]}
              spine={['hk-fxt', 'hk-akile']}
              steps={STEPS}
              nodes={AGENT_NODES.nodes}
              selected="hk-fxt"
              showHeader={false}
              readOnly={params.get('role') === 'readonly'}
            />
          </div>
        </RuleDraftScope>
      </section>
    </section>
  </QueryClientProvider>,
);
