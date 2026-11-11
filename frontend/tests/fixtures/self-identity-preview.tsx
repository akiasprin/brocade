// Production App and account identity on a synthetic fleet; no replacement UI or real requests.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import type { ConsoleBootstrap, UserListItem, Whoami } from '../../src/api';
import { theme } from '../../src/forge/theme';
import { installBackend } from './console-backend';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
if (!location.hash) history.replaceState(null, '', `${location.pathname}${location.search}#/users`);
installBackend();
const transport = globalThis.fetch;
const selfId = params.get('self') || 'grace';
const admin = params.get('role') === 'admin';
const who: Whoami = {
  operator_id: admin ? 'operator' : selfId,
  role: admin ? 'system-admin' : 'user',
  tenant_scope: admin ? null : 'platform',
  token_prefix: null,
  masked_assets: !admin,
  ...(!admin ? { self_user: { tenant_id: 'platform', user_id: selfId } } : {}),
};
const isSelf = (user: UserListItem) => user.tenant_id === 'platform' && user.id === selfId;
globalThis.fetch = async (input, init) => {
  const path = new URL(input instanceof Request ? input.url : String(input), location.origin).pathname;
  if (/^\/users\/[^/]+\/[^/]+\/traffic-policy$/.test(path)) return Response.json([]);
  if (/^\/users\/[^/]+\/[^/]+\/source-access$/.test(path)) return Response.json({ revision: 0, ips: [], nodes: [] });
  if (path === '/whoami') return Response.json(who);
  if (path === '/bootstrap') {
    const body = (await (await transport(input, init)).json()) as ConsoleBootstrap;
    return Response.json({ ...body, who });
  }
  if (path === '/me/user') {
    const body = (await (await transport('/users')).json()) as { users: UserListItem[] };
    return Response.json(body.users.find(isSelf));
  }
  if (!admin && path === '/users') {
    const body = (await (await transport(input, init)).json()) as { users: UserListItem[] };
    return Response.json({ users: body.users.map(user => (isSelf(user) ? user : { ...user, uuid: undefined })) });
  }
  return transport(input, init);
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <App />
  </QueryClientProvider>,
);
