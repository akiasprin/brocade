// Production user quota page, backed only by synthetic API responses.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type {
  AdminRole,
  ConsoleSnapshot,
  SnapshotIngress,
  UserCycleUsage,
  UserListItem,
  UserTrafficPolicy,
  UserTrafficPolicyView,
} from '../../src/api';
import { theme } from '../../src/forge/theme';
import { UsersPane } from '../../src/panes/users';
import { userTrafficPolicyKey } from '../../src/panes/user-traffic-policy';
import { SessionProvider } from '../../src/session';
import type { Win } from '../../src/wm/store';
import { MACHINES, SCENE_KEYS, SCENES, exhaustedOf, viewOf, withdrawnOf, type World } from './user-quota-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;
const tenant = 'preview';
const SELECTED = 'alice';
const POLICY_PATH = `/users/${tenant}/${SELECTED}/traffic-policy`;
const running = params.get('run') !== '0';
const OFFSET = 8 * 3_600_000;
const today = new Date(Date.now() + OFFSET);
const cycleStart = new Date(Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), 1) - OFFSET).toISOString();
const cycleEnd = new Date(Date.UTC(today.getUTCFullYear(), today.getUTCMonth() + 1, 1) - OFFSET).toISOString();

const live = {
  world: SCENES[pick(params.get('scene'), SCENE_KEYS, 'all')].world(),
  client: null as QueryClient | null,
};
let timers: number[] = [];

const userItem = (id: string, extra: Partial<UserListItem> = {}): UserListItem => ({
  tenant_id: tenant,
  id,
  status: 'active',
  created_at: '2026-01-01T00:00:00Z',
  created_revision: 1,
  uuid: '6f1c2d3e-4b5a-4c6d-8e7f-90a1b2c3d4e5',
  login_enabled: true,
  traffic_accounting_mode: 'single',
  traffic_reset_day: 1,
  service_state: 'active',
  service_starts_at: null,
  service_expires_at: null,
  ...extra,
});
const usersOf = (world: World) => [
  userItem(SELECTED, { service_state: world.serviceState, service_expires_at: world.expiresAt }),
  userItem('carol', { account_type: 'test' }),
  userItem('dave'),
];

const snapshotOf = (world: World) => {
  const users = usersOf(world);
  return {
    snapshot: {
      revision: 7,
      apps: world.lines.map(item => {
        const withdrawn = new Set(withdrawnOf(item));
        return {
          id: item.id,
          label: item.label,
          chains: item.chains.map(chain => ({ id: chain.id, name: chain.name, subscription_country: chain.country })),
          steps: [],
          fronts: [],
          ingresses: item.chains.map(
            (chain, index) =>
              ({ id: `in-${chain.id}`, chain: chain.id, node: chain.node, port: 8440 + index }) as SnapshotIngress,
          ),
          // 被配额执行器停用的接入点会撤销授权，名称列靠 quotas.suspended_ingresses 认出它们。
          grants: users.flatMap(user =>
            item.chains
              .filter(chain => user.id !== SELECTED || !withdrawn.has(`in-${chain.id}`))
              .map(chain => ({ tenant, user: user.id, ingress: `in-${chain.id}` })),
          ),
        };
      }),
      nodes: MACHINES.map(machine => ({ id: machine.node_id, name: machine.name })),
    },
    node_egress_dns: [],
    redacted: false,
  } as unknown as ConsoleSnapshot;
};
const quotasOf = (world: World) => ({
  quotas: world.lines
    .filter(item => item.quota !== null)
    .map(item => ({
      tenant_id: tenant,
      user_id: SELECTED,
      app_id: item.id,
      limit_bytes: item.quota,
      updated_at: '2026-10-01T00:00:00Z',
      suspended_ingresses: withdrawnOf(item),
    })),
});
const split = (total: number) => {
  const uplink = Math.round(total * 0.03);
  return { uplink_bytes: uplink, downlink_bytes: Math.round(total) - uplink };
};
const cyclesOf = (world: World): UserCycleUsage => {
  const users = usersOf(world);
  return {
    periods: users.map(user => ({ tenant_id: tenant, user_id: user.id, starts_at: cycleStart, ends_at: cycleEnd })),
    views: [
      ...world.lines.map(item => ({
        tenant_id: tenant,
        user_id: SELECTED,
        app_id: item.id,
        ...split(item.used),
        has_gap: false,
      })),
      ...users
        .filter(user => user.id !== SELECTED)
        .map((user, index) => ({
          tenant_id: tenant,
          user_id: user.id,
          app_id: world.lines[0].id,
          ...split((40 + index * 30) * 1024 ** 3),
          has_gap: false,
        })),
    ],
    forwarding: [],
    traffic_policies: world.lines
      .filter(item => item.action)
      .map(item => ({
        tenant_id: tenant,
        user_id: SELECTED,
        kind: 'access' as const,
        app_id: item.id,
        quota_bytes: item.quota === null ? null : String(Math.round(item.quota)),
        used_bytes: String(Math.round(item.used)),
        exhausted: exhaustedOf(item),
        suspended: withdrawnOf(item).length > 0,
        rate_bps: item.action?.kind === 'throttle' ? item.action.rate_bps : null,
      })),
  } as UserCycleUsage;
};
const viewsOf = (world: World): UserTrafficPolicyView[] => [
  ...world.lines.map(item => viewOf(world, item)),
  {
    kind: 'forwarding',
    app_id: null,
    policy: null,
    used_bytes: '0',
    exhausted: false,
    service_available: world.serviceAvailable,
    unsupported_nodes: [],
    pending_nodes: [],
  },
];
const nodesList = { nodes: MACHINES.map(machine => ({ node_id: machine.node_id, name: machine.name })) };

/** Publish the synthetic API change to the production query cache. */
function commit(world: World) {
  live.world = world;
  const client = live.client;
  if (client) {
    client.setQueryData(['snapshot'], snapshotOf(world));
    client.setQueryData(['users'], { users: usersOf(world) });
    client.setQueryData(['quotas'], quotasOf(world));
    client.setQueryData(['user-cycles'], cyclesOf(world));
    client.setQueryData(userTrafficPolicyKey(usersOf(world)[0]), viewsOf(world));
  }
}

/** 限速策略保存后，相关机器先列为待同步，随后逐台确认。 */
function converge() {
  timers.forEach(timer => clearTimeout(timer));
  timers = [];
  if (!running) return;
  const step = (delay: number) =>
    timers.push(
      window.setTimeout(() => {
        let done = false;
        const lines = live.world.lines.map(item => {
          if (done || item.pending.length === 0) return item;
          done = true;
          return { ...item, pending: item.pending.slice(1) };
        });
        commit({ ...live.world, lines });
      }, delay),
    );
  step(1600);
  step(3400);
  step(5200);
}

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), location.origin);
  const path = url.pathname;
  const method = init?.method ?? 'GET';
  const world = live.world;
  if (path.startsWith(`${POLICY_PATH}/access/`) && method === 'PUT') {
    await new Promise(resolve => setTimeout(resolve, 300));
    const app = decodeURIComponent(path.slice(`${POLICY_PATH}/access/`.length));
    const body = JSON.parse(String(init?.body)) as UserTrafficPolicy;
    const lines = world.lines.map(item => {
      if (item.id !== app) return item;
      const action = body.exhausted_action;
      const changedRate =
        action.kind === 'throttle' && (item.action?.kind !== 'throttle' || item.action.rate_bps !== action.rate_bps);
      return {
        ...item,
        quota: body.quota_bytes === null ? null : Number(body.quota_bytes),
        action,
        pending:
          action.kind !== 'throttle'
            ? []
            : changedRate
              ? [...new Set(item.chains.map(chain => chain.node))]
              : item.pending,
      };
    });
    commit({ ...world, lines });
    converge();
    return Response.json(viewsOf(live.world));
  }
  if (path === POLICY_PATH) return Response.json(viewsOf(world));
  if (path.endsWith('/traffic-policy')) return Response.json([]);
  if (path === '/users') return Response.json({ users: usersOf(world) });
  if (path === '/usage/user-cycles') return Response.json(cyclesOf(world));
  if (path === '/quotas') return Response.json(quotasOf(world));
  if (path === '/model/snapshot') return Response.json(snapshotOf(world));
  if (path === '/tenants') return Response.json({ tenants: [{ id: tenant, name: '示例' }] });
  if (path === '/nodes/agent-state') return Response.json(nodesList);
  if (path === '/users/presence') return Response.json({ freshness_secs: 120, users: [] });
  if (path.endsWith('/presence-history'))
    return Response.json({ tenant_id: tenant, user_id: SELECTED, retention_days: 30, truncated: false, sources: [] });
  if (path.endsWith('/source-access')) return Response.json({ revision: 0, ips: [], nodes: [] });
  if (path.endsWith('/forwarding'))
    return Response.json({ tenant_id: tenant, user_id: SELECTED, entitlements: [], rules: [] });
  if (path.endsWith('/tcp-pings')) return Response.json([]);
  if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

function seededClient(world: World, role: AdminRole): QueryClient {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
  });
  client.setQueryData(['snapshot'], snapshotOf(world));
  client.setQueryData(['nodes'], nodesList);
  client.setQueryData(['users'], { users: usersOf(world) });
  client.setQueryData(['tenants'], { tenants: [{ id: tenant, name: '示例' }] });
  client.setQueryData(['quotas'], quotasOf(world));
  client.setQueryData(['user-cycles'], cyclesOf(world));
  client.setQueryData(['user-presence', role], { freshness_secs: 120, users: [] });
  client.setQueryData(userTrafficPolicyKey(usersOf(world)[0]), viewsOf(world));
  return client;
}

const win: Win = {
  id: 1,
  key: 'tab:users',
  title: '用户',
  x: 0,
  y: 0,
  w: 1200,
  h: 900,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'user', id: SELECTED } },
};

const role: AdminRole = params.get('role') === 'readonly' ? 'readonly' : 'system-admin';
const client = seededClient(live.world, role);
live.client = client;
if (live.world.lines.some(item => item.pending.length > 0)) converge();
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: { operator_id: 'operator', role, tenant_scope: null, token_prefix: null, masked_assets: false },
        initial: { node_count: MACHINES.length, chain_group_count: [] },
      }}
    >
      <UsersPane win={win} />
    </SessionProvider>
  </QueryClientProvider>,
);
