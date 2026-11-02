// Synthetic Console for the rate-limit demo. No request reaches a Console or an Agent.
import { QueryClient } from '@tanstack/react-query';
import type {
  ConsoleSnapshot,
  SnapshotIngress,
  UserCycleUsage,
  UserListItem,
  UserTrafficPolicyView,
} from '../../src/api';
import type { Session } from '../../src/session';
import { userTrafficPolicyKey } from '../../src/panes/user-traffic-policy';

export const GiB = 1024 ** 3;
export type Policy = {
  quota: number | null;
  baseRate: number | null;
  action: 'suspend' | 'throttle';
  afterRate: number;
};
export type Group = {
  id: string;
  label: string;
  countries: string[];
  chains: string[];
  used: number;
  policy: Policy;
  pending: boolean;
};
type World = { cycle: number; groups: Record<string, Group[]> };
const tenant = 'preview';
export const users: UserListItem[] = ['alice', 'carol', 'dave', 'erin', 'frank'].map((id, i) => ({
  tenant_id: tenant,
  id,
  status: 'active',
  created_at: '2026-10-01T00:00:00Z',
  created_revision: 1,
  uuid: `00000000-0000-4000-8000-${String(i + 1).padStart(12, '0')}`,
  login_enabled: true,
  traffic_accounting_mode: 'single',
  traffic_reset_day: 1,
  service_state: 'active',
  service_starts_at: null,
  service_expires_at: null,
  ...(id === 'carol' ? { account_type: 'test' as const } : {}),
}));
export const session: Session = {
  who: { operator_id: 'rate-demo', role: 'system-admin', tenant_scope: null, token_prefix: null, masked_assets: false },
  initial: { node_count: 3, chain_group_count: [] },
};
const groups = (): Group[] => [
  {
    id: 'apac-premium',
    label: 'APAC Premium',
    countries: ['HK', 'JP', 'TW'],
    chains: ['香港-01', '东京-01', '台北-01'],
    used: 121.6,
    policy: { quota: 500, baseRate: 100, action: 'throttle', afterRate: 5 },
    pending: false,
  },
  {
    id: 'apac-promising',
    label: 'APAC Promising',
    countries: ['HK', 'SG'],
    chains: ['香港-02', '新加坡-01'],
    used: 46.2,
    policy: { quota: null, baseRate: 50, action: 'suspend', afterRate: 5 },
    pending: false,
  },
  {
    id: 'apac-standard',
    label: 'APAC Standard',
    countries: ['JP', 'SG'],
    chains: ['东京-02', '新加坡-02'],
    used: 87.4,
    policy: { quota: 1000, baseRate: null, action: 'suspend', afterRate: 5 },
    pending: false,
  },
  {
    id: 'noram-premium',
    label: 'NORAM Premium',
    countries: ['US', 'CA'],
    chains: ['洛杉矶-01', '多伦多-01'],
    used: 12.8,
    policy: { quota: 200, baseRate: 80, action: 'throttle', afterRate: 5 },
    pending: false,
  },
  {
    id: 'noram-standard',
    label: 'NORAM Standard',
    countries: ['US'],
    chains: ['圣何塞-01'],
    used: 23.1,
    policy: { quota: null, baseRate: null, action: 'suspend', afterRate: 5 },
    pending: false,
  },
  {
    id: 'emea-premium',
    label: 'EMEA Premium',
    countries: ['DE', 'GB'],
    chains: ['法兰克福-01', '伦敦-01'],
    used: 5.6,
    policy: { quota: 100, baseRate: null, action: 'suspend', afterRate: 5 },
    pending: false,
  },
  {
    id: 'vpngate',
    label: 'VPN Gate / Proxies',
    countries: ['JP'],
    chains: ['公共出口'],
    used: 1.4,
    policy: { quota: null, baseRate: 20, action: 'suspend', afterRate: 5 },
    pending: false,
  },
];
let world: World = {
  cycle: 0,
  groups: Object.fromEntries(
    users.map((user, index) => [
      user.id,
      groups().map(group => ({ ...group, used: index === 0 ? group.used : group.used / (index + 1) })),
    ]),
  ),
};
const listeners = new Set<() => void>();
export const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};
export const snapshot = () => world;
export const exhausted = (group: Group) => group.policy.quota !== null && group.used >= group.policy.quota;
export function effective(group: Group): number | 'suspended' | null {
  const { baseRate, action, afterRate } = group.policy;
  if (!exhausted(group)) return baseRate;
  if (action === 'suspend') return 'suspended';
  return Math.min(baseRate ?? Infinity, afterRate);
}
export function viewOf(group: Group): UserTrafficPolicyView {
  const rate = effective(group);
  return {
    kind: 'access',
    app_id: group.id,
    policy:
      group.policy.quota === null && group.policy.baseRate === null
        ? null
        : {
            quota_bytes: group.policy.quota === null ? null : String(group.policy.quota * GiB),
            base_rate_bps: group.policy.baseRate === null ? null : group.policy.baseRate * 1_000_000,
            exhausted_action:
              group.policy.action === 'suspend'
                ? { kind: 'suspend' }
                : {
                    kind: 'throttle',
                    rate_bps: group.policy.afterRate * 1_000_000,
                  },
          },
    used_bytes: String(Math.round(group.used * GiB)),
    exhausted: exhausted(group),
    service_available: true,
    unsupported_nodes: [],
    pending_nodes: group.pending ? ['香港入口', '东京入口'] : [],
    base_rate_supported: true,
    effective:
      rate === 'suspended'
        ? { mode: 'suspended' }
        : rate === null
          ? { mode: 'unlimited', managed: group.policy.quota !== null && group.policy.action === 'throttle' }
          : { mode: 'limited', rate_bps: rate * 1_000_000 },
  };
}
const nodes = {
  nodes: [
    { node_id: 'n1', name: '香港入口' },
    { node_id: 'n2', name: '东京入口' },
    { node_id: 'n3', name: '台北入口' },
  ],
};
export const model = {
  snapshot: {
    revision: 1,
    nodes: nodes.nodes.map(node => ({ id: node.node_id, name: node.name })),
    apps: groups().map(group => ({
      id: group.id,
      label: group.label,
      chains: group.chains.map((name, i) => ({
        id: `${group.id}-${i}`,
        name,
        subscription_country: group.countries[i % group.countries.length],
      })),
      steps: [],
      fronts: [],
      ingresses: group.chains.map(
        (_, i) =>
          ({
            id: `in-${group.id}-${i}`,
            chain: `${group.id}-${i}`,
            node: `n${(i % 3) + 1}`,
            port: 8443 + i,
          }) as SnapshotIngress,
      ),
      grants: users.flatMap(user =>
        group.chains.map((_, i) => ({ tenant, user: user.id, ingress: `in-${group.id}-${i}` })),
      ),
    })),
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;
const quotas = () => ({
  quotas: users.flatMap(user =>
    world.groups[user.id]
      .filter(group => group.policy.quota !== null)
      .map(group => ({
        tenant_id: tenant,
        user_id: user.id,
        app_id: group.id,
        limit_bytes: group.policy.quota! * GiB,
        updated_at: '2026-10-01T00:00:00Z',
        suspended_ingresses: [],
      })),
  ),
});
const cycles = (): UserCycleUsage => ({
  periods: users.map(user => ({
    tenant_id: tenant,
    user_id: user.id,
    starts_at: new Date(Date.UTC(2026, 9 + world.cycle, 1, -8)).toISOString(),
    ends_at: new Date(Date.UTC(2026, 10 + world.cycle, 1, -8)).toISOString(),
  })),
  views: users.flatMap(user =>
    world.groups[user.id].map(group => ({
      tenant_id: tenant,
      user_id: user.id,
      app_id: group.id,
      uplink_bytes: Math.round(group.used * GiB * 0.03),
      downlink_bytes: Math.round(group.used * GiB * 0.97),
      has_gap: false,
    })),
  ),
  forwarding: [],
  traffic_policies: users.flatMap(user =>
    world.groups[user.id].map(group => ({
      tenant_id: tenant,
      user_id: user.id,
      kind: 'access' as const,
      app_id: group.id,
      quota_bytes: group.policy.quota === null ? null : String(group.policy.quota * GiB),
      used_bytes: String(Math.round(group.used * GiB)),
      exhausted: exhausted(group),
      suspended: effective(group) === 'suspended',
      rate_bps: typeof effective(group) === 'number' ? Number(effective(group)) * 1_000_000 : null,
    })),
  ),
});
export const client = new QueryClient({
  defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
});
function sync() {
  client.setQueryData(['quotas'], quotas());
  client.setQueryData(['user-cycles'], cycles());
  users.forEach(user => client.setQueryData(userTrafficPolicyKey(user), world.groups[user.id].map(viewOf)));
  listeners.forEach(listener => listener());
}
function change(userId: string, changeGroup: (group: Group) => Group) {
  world = { ...world, groups: { ...world.groups, [userId]: world.groups[userId].map(changeGroup) } };
  sync();
}
const timers = new Map<string, number>();
export function savePolicy(userId: string, groupId: string, policy: Policy) {
  const key = `${userId}/${groupId}`;
  clearTimeout(timers.get(key));
  change(userId, group => (group.id === groupId ? { ...group, policy, pending: true } : group));
  timers.set(
    key,
    window.setTimeout(() => {
      change(userId, group => (group.id === groupId ? { ...group, pending: false } : group));
      timers.delete(key);
    }, 1600),
  );
}
export function fillQuota(userId: string) {
  change(userId, group =>
    group.id === 'apac-premium' && group.policy.quota !== null ? { ...group, used: group.policy.quota + 12.4 } : group,
  );
}
export function newCycle(userId: string) {
  world = { ...world, cycle: world.cycle + 1 };
  change(userId, group => ({ ...group, used: 0 }));
}

client.setQueryData(['snapshot'], model);
client.setQueryData(['users'], { users });
client.setQueryData(['nodes'], nodes);
client.setQueryData(['tenants'], { tenants: [{ id: tenant, name: '示例' }] });
client.setQueryData(['user-presence', session.who.role], { freshness_secs: 120, users: [] });
sync();

// A demo must never establish real event streams or pass writes through to a server.
Object.defineProperty(window, 'EventSource', { value: undefined, configurable: true });
globalThis.fetch = async (input, init) => {
  const path = new URL(input instanceof Request ? input.url : String(input), location.origin).pathname;
  const match = path.match(/^\/users\/preview\/([^/]+)\/traffic-policy\/access\/([^/]+)$/);
  if (match && init?.method === 'PUT') {
    const user = decodeURIComponent(match[1]);
    const app = decodeURIComponent(match[2]);
    const policy = JSON.parse(String(init.body)) as import('../../src/api').UserTrafficPolicy;
    savePolicy(user, app, {
      quota: policy.quota_bytes === null ? null : Number(policy.quota_bytes) / GiB,
      baseRate: policy.base_rate_bps == null ? null : policy.base_rate_bps / 1_000_000,
      action: policy.exhausted_action.kind,
      afterRate: policy.exhausted_action.kind === 'throttle' ? policy.exhausted_action.rate_bps / 1_000_000 : 5,
    });
    return Response.json(world.groups[user].map(viewOf));
  }
  if (path === '/whoami') return Response.json(session.who);
  if (path === '/users') return Response.json({ users });
  if (path === '/usage/user-cycles') return Response.json(cycles());
  if (path === '/quotas') return Response.json(quotas());
  if (path === '/model/snapshot') return Response.json(model);
  if (path === '/tenants') return Response.json({ tenants: [{ id: tenant, name: '示例' }] });
  if (path === '/nodes/agent-state') return Response.json(nodes);
  if (path === '/users/presence') return Response.json({ freshness_secs: 120, users: [] });
  if (path.endsWith('/traffic-policy')) {
    const userId = decodeURIComponent(path.split('/')[3]);
    return Response.json((world.groups[userId] ?? []).map(viewOf));
  }
  if (path.endsWith('/presence-history'))
    return Response.json({ tenant_id: tenant, user_id: 'alice', retention_days: 30, truncated: false, sources: [] });
  if (path.endsWith('/source-access')) return Response.json({ revision: 0, ips: [], nodes: [] });
  if (path.endsWith('/forwarding'))
    return Response.json({ tenant_id: tenant, user_id: 'alice', entitlements: [], rules: [] });
  if (path.endsWith('/tcp-pings')) return Response.json([]);
  if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '本地演示' });
  if (path === '/revisions') return Response.json({ current_revision: 1, revisions: [{ id: 1 }] });
  if (path === '/compile/1') return Response.json({ diagnostics: [] });
  if (path === '/deployments/verify') return Response.json({ summary: { changed_targets: 0 } });
  if (path === '/deployments') return Response.json({ deployments: [] });
  if (path === '/grants/automation')
    return Response.json({ pending_jobs: 0, retrying_jobs: 0, failed_jobs: 0, max_attempts: 0 });
  if (path === '/load/nodes/nic') return Response.json({ nodes: [] });
  if (path === '/realtime/fleet-traffic/snapshot')
    return Response.json({ policy: { enabled: false }, nodes: [], expected: [] });
  if (path === '/notifications')
    return Response.json({
      events: [],
      active: [],
      unread_count: 0,
      latest_event_id: 0,
      last_seen_event_id: 0,
      cleared_through_event_id: 0,
      retention_days: 30,
    });
  return Response.json({ error: `本地演示未提供 ${path}` }, { status: 404 });
};
