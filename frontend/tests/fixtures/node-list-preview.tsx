// Visual acceptance fixture: the production machine list (NodesPane, sheeted as the shell mounts it) on the synthetic
// fleet shared with node-list-mockup; no network access. ?scene=typical|calm|trouble|few|many|empty picks the fleet,
// ?role=system-admin|readonly|public the viewer and ?theme=dark|light the theme.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, Whoami } from '../../src/api';
import { theme } from '../../src/forge/theme';
import { NodesPane } from '../../src/panes/nodes';
import { SessionProvider } from '../../src/session';
import type { Win } from '../../src/wm/store';
import '../../src/styles.css';
import { SCENES, agentState, nicView, pingLatest, reports, trafficItem, type SceneKey } from './node-list-mockup-data';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';

const scene = params.get('scene') as SceneKey | null;
const machines = (scene && SCENES[scene] ? SCENES[scene] : SCENES.typical).machines();
const role = params.get('role') ?? 'system-admin';
const who: Whoami =
  role === 'public'
    ? { operator_id: 'public', role: 'readonly', tenant_scope: null, token_prefix: null, masked_assets: true }
    : {
        operator_id: 'operator',
        role: role as AdminRole,
        tenant_scope: null,
        token_prefix: null,
        masked_assets: role === 'readonly',
      };
// Masked roles receive the server's masked form; documentation-range addresses keep the layout realistic.
const masked = (ip: string | null) =>
  !who.masked_assets || !ip
    ? ip
    : ip.includes(':')
      ? `${ip.split(':').slice(0, 2).join(':')}:***`
      : ip.replace(/\.\d+\.\d+$/, '.***.***');

const json = (body: unknown, status = 200) => Promise.resolve(Response.json(body, { status }));
window.fetch = ((input: RequestInfo | URL) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const now = Math.floor(Date.now() / 1000);
  switch (url.pathname) {
    case '/nodes/agent-state':
      return json({
        nodes: machines.map(machine => {
          const state = agentState(machine, now);
          return { ...state, public_ipv4: masked(state.public_ipv4), public_ipv6: masked(state.public_ipv6) };
        }),
      });
    case '/node-traffic':
      return json({ nodes: machines.map(machine => trafficItem(machine, now)) });
    case '/load/nodes/nic':
      return json({
        nodes: machines
          .filter(reports)
          .map(machine => nicView(machine, now, Number(url.searchParams.get('windows')) || 24)),
      });
    case '/ping-probe/nodes/latest':
      return json({ interval_secs: 10, nodes: machines.flatMap(machine => pingLatest(machine, now) ?? []) });
    default:
      // Hover prefetches machine details; the list itself never needs them.
      return json({ error: 'not part of the machine list fixture' }, 404);
  }
}) as typeof fetch;

const win: Win = {
  id: 9001,
  key: 'tab:nodes',
  title: '机器',
  x: 0,
  y: 0,
  w: 1200,
  h: 900,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'list' } },
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider value={{ who, initial: { node_count: machines.length, chain_group_count: [] } }}>
      <NodesPane win={win} bare />
    </SessionProvider>
  </QueryClientProvider>,
);
