import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import '../../src/styles.css';
import { installBackend } from './console-backend';
import { theme } from '../../src/forge/theme';
import { probeTunnel, probeView } from './tunnel-probe-data';
import { draft } from '../../src/draft';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
if (params.get('scene') === 'draft-changed') {
  draft.init('admin');
  const { tenant, ...outbound } = probeTunnel;
  draft.push({ op: 'upsert_external_outbound', outbound: { ...outbound, tenant_id: tenant } });
}
installBackend();
const backend = globalThis.fetch;
globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const scene = params.get('scene') ?? 'healthy';
  if (url.pathname === '/model/preview' && scene === 'draft-changed') {
    const snapshot = await (await globalThis.fetch('/model/snapshot')).json();
    return Response.json({
      snapshot,
      compile: { diagnostics: [], summary: {}, system: { nodes: [] }, apps: [] },
      artifacts: { revision: snapshot.snapshot.revision, artifacts: [] },
    });
  }
  if (url.pathname === '/tunnel-probes/capability') return Response.json({ available: true, reason: null });
  if (url.pathname === '/tunnel-probes')
    return Response.json({
      origin: 'console',
      endpoint_url: 'https://probe.example.test/cdn-cgi/trace',
      retention_days: 7,
      items: [probeView(86400, scene).item],
    });
  if (url.pathname.endsWith(`/tunnels/${probeTunnel.id}/probe`)) {
    if (scene === 'unpublished') return Response.json({ error: '未发布' }, { status: 404 });
    if (scene === 'error') return Response.json({ error: '拨测记录读取失败' }, { status: 503 });
    return Response.json(probeView(Number(url.searchParams.get('window_secs') ?? 86400), scene));
  }
  const response = await backend(input, init);
  if (url.pathname === '/model/snapshot') {
    const value = await response.json();
    value.snapshot.external_outbounds[1] = probeTunnel;
    const app = value.snapshot.apps[0];
    app.chains.push({ id: 'c-probe-stream', tenant: 'platform', name: '流媒体 · 新加坡' });
    const references =
      scene === 'no-references'
        ? []
        : [
            ['c-hk-direct', 'hk-hkix'],
            ['c-probe-stream', 'hk-bgp'],
            ['c-probe-stream', 'jp-iij'],
          ];
    if (scene === 'long-references') {
      app.label = '跨区域流媒体和办公访问项目 / international network';
      for (const chain of app.chains)
        if (['c-hk-direct', 'c-probe-stream'].includes(chain.id))
          chain.name = `${chain.name} · 跨区域备用线路和长名称展示检查`;
    }
    for (const [chain, node] of references)
      app.steps.push({
        chain,
        node,
        accept: {},
        hop_in: null,
        rules: [{ m: { t: 'any' }, a: { t: 'proxy', outbound: probeTunnel.id } }],
      });
    return Response.json(value);
  }
  if (url.pathname === '/nodes/agent-state' && scene === 'long-references') {
    const value = await response.json();
    for (const node of value.nodes)
      if (['hk-hkix', 'hk-bgp', 'jp-iij'].includes(node.node_id))
        node.name = `${node.name} · 多地区运营商互联机器节点长名称检查`;
    return Response.json(value);
  }
  return response;
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <App />
  </QueryClientProvider>,
);
