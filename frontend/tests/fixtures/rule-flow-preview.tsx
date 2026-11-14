import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { chainSpine } from '../../src/api';
import { ChainRulesPanel } from '../../src/panes/chains';
import { RuleDraftScope } from '../../src/panes/rules';
import { PanelTitle } from '../../src/ui/icons';
import { draft } from '../../src/draft';
import { theme } from '../../src/forge/theme';
import { APP_ID, COMPILED, NODES, SETTINGS, SNAPSHOT } from './rule-flow-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
const snapshot = params.has('covered-forward') ? structuredClone(SNAPSHOT) : SNAPSHOT;
const compiled = params.has('covered-forward') ? structuredClone(COMPILED) : COMPILED;
if (params.has('covered-forward')) {
  const rules = snapshot.snapshot.apps[0].steps[0].rules;
  rules[1].a = structuredClone(rules[0].a);
  const rendered = compiled[0].steps![0].rules;
  rendered[1].action = structuredClone(rendered[0].action);
}
const desiredTheme = params.get('theme') === 'dark' ? 'dark' : 'light';
if (theme.snapshot() !== desiredTheme) theme.toggle();
draft.init('rule-flow-acceptance');
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['snapshot'], snapshot);
client.setQueryData(['nodes'], { nodes: NODES });
client.setQueryData(['settings'], SETTINGS);
client.setQueryData(['revisions'], { current_revision: 1, revisions: [] });
client.setQueryData(['compile', 1], { apps: compiled, diagnostics: [], system: { nodes: [] } });
globalThis.fetch = async input => {
  const url = new URL(String(input), location.origin);
  if (url.pathname === '/routing/geosite') {
    if (url.searchParams.get('domain') === 'unavailable.example')
      return new Response(JSON.stringify({ error: '统一 geosite 规则集暂时无法加载' }), { status: 503 });
    return new Response(
      JSON.stringify({
        matches: {
          'category-ai-!cn': { hit: url.searchParams.get('domain') === 'openai.com' },
        },
      }),
    );
  }
  throw new Error(`合成验收未提供该接口：${String(input).split('?')[0]}`);
};
const app = snapshot.snapshot.apps[0];
const machine = params.has('node');
const chains = machine ? app.chains.slice(0, 7) : app.chains.slice(0, 1);
const readOnly = params.has('readonly');
const panels = chains.map(chain => (
  <div className="node-chain-use" key={chain.id}>
    <ChainRulesPanel
      appId={APP_ID}
      chain={chain}
      spine={chainSpine(app, chain.id)}
      steps={app.steps.filter(step => step.chain === chain.id)}
      nodes={NODES}
      selected="hk-fxt"
      machineRole={machine ? '入口节点' : undefined}
      showHeader={false}
      readOnly={readOnly}
      settingsReadable={!readOnly}
    />
  </div>
));
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <section className="panel config-panel rule-sheet-card node-chain-sheet">
      <header>
        <PanelTitle of="chains">链路规则</PanelTitle>
      </header>
      {readOnly ? panels : <RuleDraftScope>{panels}</RuleDraftScope>}
    </section>
  </QueryClientProvider>,
);
