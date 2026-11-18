import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { theme } from '../../src/forge/theme';
import type { DeploymentTargetDetail } from '../../src/api';
import '../../src/styles.css';
const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'dark' ? 'dark' : 'light')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
// The optional module is used only to compare the same fixture with the pre-change component.
const modulePath = params.get('module') ?? '/src/panes/deploy.tsx';
const { RecordedArtifacts } = await import(/* @vite-ignore */ modulePath);
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['nodes'], { nodes: [{ node_id: 'home-dell', name: '家中 DELL · 开发宿主' }] });
const target: DeploymentTargetDetail = {
  node_id: 'home-dell',
  status: 'succeeded',
  error: null,
  wave: 1,
  disruptive: true,
  observed_before: { xray: { state: 'present', sha256: 'a'.repeat(64), content: '{\n  "port": 8443\n}\n' } },
  observed_after: null,
  verdict: null,
  dispatched_at: null,
  desired_structure: {
    actions: ['apply-xray', 'sync-cvm'],
    xray: { state: 'present', sha256: 'b'.repeat(64), content: '{\n  "port": 443\n}\n' },
    cvm: {
      revision_id: 42,
      network_bindings: 2,
      entries: 1,
      instances: [
        {
          id: 'cvm-demo',
          name: 'Ubuntu 开发环境',
          generation: 7,
          power: 'running',
          files: [{ kind: 'cube/egress.json', sha256: 'c'.repeat(64), before_absent: true }],
        },
      ],
    },
  },
};
globalThis.fetch = async input => {
  if (String(input) === '/cvm/cvm-demo/artifacts?generation=7')
    return new Response(
      JSON.stringify({
        generation: 7,
        previous: null,
        current: {
          files: [
            {
              kind: 'cube/egress.json',
              sha256: 'c'.repeat(64),
              content: '{\n  "policy": "deny-by-default",\n  "routes": []\n}\n',
            },
          ],
        },
      }),
      { headers: { 'content-type': 'application/json' } },
    );
  throw new Error(`unexpected fixture request: ${String(input)}`);
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <main>
      <h1>配置发布 #18 · R42</h1>
      <p className="note">机器和容器共用全局修订 · 本次已完成</p>
      <RecordedArtifacts targets={[target]} range="R41 → R42" />
    </main>
  </QueryClientProvider>,
);
