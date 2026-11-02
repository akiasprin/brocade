// Mount the production app on synthetic API data. No alternate navigation or styles.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { theme } from '../../src/forge/theme';
import { installBackend } from './console-backend';
import { settings } from './unlock-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
installBackend();
const backend = globalThis.fetch;
globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const state = params.get('state') ?? 'idle';
  if (url.pathname === '/settings') return Response.json(settings);
  if (url.pathname === '/tunnel-probes/capability')
    return Response.json({ available: false, reason: '本地夹具不拨测' });
  if (url.pathname.endsWith('/tunnels/custom-1234-5678/probe'))
    return Response.json({ error: '隧道尚无探测记录' }, { status: 404 });
  if (url.pathname === '/deployments' && url.searchParams.has('active_only'))
    return Response.json({
      deployments: state === 'deploying' ? [{ id: 42, active: true, awaiting_confirmation: false }] : [],
    });
  const response = await backend(input, init);
  if (url.pathname === '/model/snapshot') {
    const value = await response.json();
    // Canonical tunnel IDs exercise real detail routes rather than legacy fixture IDs.
    value.snapshot.external_outbounds[1].id = 'custom-1234-5678';
    value.snapshot.external_outbounds[1].protocol = { t: 'socks5', v: { username: '', credential: '' } };
    value.snapshot.external_outbounds[1].security = { t: 'none' };
    return Response.json(value);
  }
  if (url.pathname === '/deployments/verify' && state === 'pending') {
    const value = await response.json();
    value.summary.changed_targets = 3;
    return Response.json(value);
  }
  if (url.pathname === '/grants/automation' && state === 'grant') {
    const value = await response.json();
    value.failed_jobs = 2;
    value.last_error = '合成权限任务失败';
    return Response.json(value);
  }
  if (url.pathname === '/bootstrap') {
    const value = await response.json();
    value.who.operator_id = params.get('account') ?? 'admin';
    if (params.get('role') === 'readonly') value.who.role = 'readonly';
    if (params.get('role') === 'public') {
      value.who.role = 'readonly';
      value.who.operator_id = 'public';
    }
    return Response.json(value);
  }
  return response;
};

createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <App />
  </QueryClientProvider>,
);
