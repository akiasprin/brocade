// Visual acceptance fixture: the production users pane with synthetic users only; no network access.
// ?user=alice|carol|erin chooses the detail, ?role=system-admin|readonly|user the viewer, ?theme=dark|light,
// ?presence=sources gives alice online and recent sources (one of them banned) instead of an empty presence.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, ConsoleSnapshot, SnapshotIngress, UserListItem, UserOnlineSource } from '../../src/api';
import { UsersPane } from '../../src/panes/users';
import { SessionProvider } from '../../src/session';
import { theme } from '../../src/forge/theme';
import type { Win } from '../../src/wm/store';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = params.get('palette') === 'jinzi' ? 'jinzi' : 'dailan';
const role = (params.get('role') ?? 'system-admin') as AdminRole;
const selected = params.get('user') ?? 'alice';
const tenant = 'preview';
const GiB = 1024 ** 3;
const trafficState = params.get('traffic') ?? 'normal';
const person = (id: string, extra: Partial<UserListItem> = {}): UserListItem => ({
  tenant_id: tenant,
  id,
  status: 'active',
  created_at: '2026-10-01T00:00:00Z',
  created_revision: 1,
  uuid: '00000000-0000-4000-8000-000000000000',
  login_enabled: true,
  ...extra,
});
const users = [
  person('alice', { direct_login_enabled: true }),
  person('carol', { account_type: 'test' }),
  person('a-very-long-user-name-for-layout', {}),
  person('erin', { status: 'disabled' }),
  person('pending', { service_state: 'pending' }),
  person('expired', { service_state: 'expired' }),
];
const chains = ['hk-01', 'jp-01', 'tw-01'];
const snapshot = {
  snapshot: {
    revision: 1,
    apps: [
      {
        id: 'apac',
        label: 'APAC Premium',
        chains: chains.map(id => ({ id, name: id.toUpperCase(), subscription_country: id.slice(0, 2).toUpperCase() })),
        steps: [],
        fronts: [],
        ingresses: chains.map(
          (chain, i) => ({ id: `in-${chain}`, chain, node: 'n1', port: 8448 + i }) as SnapshotIngress,
        ),
        grants: users.flatMap(user => chains.map(chain => ({ tenant, user: user.id, ingress: `in-${chain}` }))),
      },
    ],
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;
if (params.get('apps') === 'two')
  snapshot.snapshot.apps.push({
    id: 'budget',
    label: 'JP Budget',
    chains: [{ id: 'jp-budget', name: 'JP Budget', subscription_country: 'JP' }],
    steps: [],
    fronts: [],
    ingresses: [{ id: 'in-budget', chain: 'jp-budget', node: 'n1', port: 8555 } as SnapshotIngress],
    grants: users.map(user => ({ tenant, user: user.id, ingress: 'in-budget' })),
  });
const appExhausted = (app: string) => app !== 'budget' && ['exhausted', 'stopped', 'extreme'].includes(trafficState);
const appLimit = (app: string) =>
  app === 'budget' ? 50 : trafficState === 'extreme' ? 1 : appExhausted(app) ? 100 : 500;
const policyFor = (app: string) => ({
  base_rate_supported: true,
  kind: 'access',
  app_id: app,
  policy: {
    quota_bytes: String(appLimit(app) * GiB),
    exhausted_action:
      app === 'budget' || trafficState === 'stopped' ? { kind: 'suspend' } : { kind: 'throttle', rate_bps: 5_000_000 },
  },
  used_bytes: String((app === 'budget' ? 12 : 121) * GiB),
  exhausted: appExhausted(app),
  service_available: true,
  unsupported_nodes: trafficState === 'unsupported' ? ['示例节点'] : [],
  pending_nodes: ['pending', 'extreme'].includes(trafficState) ? ['示例节点'] : [],
});
// Documentation-range addresses; regions and operators are made up for layout.
const MINUTE = 60_000;
const presenceSource = (
  ip: string,
  chain: string,
  minutesAgo: number,
  online: boolean,
  gib: number,
): UserOnlineSource => {
  const last = new Date(Date.now() - (online ? 0.3 : minutesAgo) * MINUTE).toISOString();
  const since = new Date(Date.now() - minutesAgo * MINUTE - (online ? 0 : 50 * MINUTE)).toISOString();
  const access = {
    node_id: 'n1',
    ingress_id: `in-${chain}`,
    protocols: ['vless' as const],
    last_observed_at: last,
    xray_last_seen_at: last,
    online_since_at: since,
    session_uplink_bytes: Math.round(gib * 0.04 * GiB),
    session_downlink_bytes: Math.round(gib * 0.96 * GiB),
  };
  return {
    ip,
    first_observed_at: since,
    last_observed_at: last,
    xray_last_seen_at: last,
    online_since_at: since,
    session_uplink_bytes: access.session_uplink_bytes,
    session_downlink_bytes: access.session_downlink_bytes,
    node_ids: ['n1'],
    ingress_ids: [access.ingress_id],
    accesses: [access],
  };
};
const presenceState = params.get('presence');
const incompletePresence = presenceState === 'partial' || presenceState === 'unavailable';
const withSources = presenceState === 'sources' || incompletePresence;
const onlineSources = withSources
  ? [presenceSource('198.51.100.77', 'tw-01', 371, true, 8.4), presenceSource('203.0.113.24', 'hk-01', 134, true, 1.2)]
  : [];
const historySources = withSources
  ? [
      presenceSource('203.0.113.61', 'hk-01', 140, false, 0.4),
      presenceSource('2001:db8:4c1a:20:5a2f:1bff:fe33:c901', 'jp-01', 26 * 60, false, 0.7),
      presenceSource('203.0.113.17', 'jp-01', 4 * 24 * 60 + 70, false, 14.2),
    ]
  : [];
if (params.has('sessionHistory')) {
  historySources.push(
    presenceSource('198.51.100.77', 'tw-01', 14 * 60, false, 2.4),
    presenceSource('198.51.100.77', 'hk-01', 14 * 60 + 20, false, 0.1),
    presenceSource('198.51.100.77', 'tw-01', 14 * 60 + 59, false, 0.2),
    presenceSource('198.51.100.77', 'hk-01', 28 * 60, false, 0.2),
    presenceSource('203.0.113.61', 'jp-01', 2 * 24 * 60, false, 0.8),
    {
      ...presenceSource('198.51.100.77', 'jp-01', 3 * 24 * 60, false, 0),
      online_since_at: null,
      session_uplink_bytes: null,
      session_downlink_bytes: null,
    },
  );
  historySources.forEach((source, index) => {
    source.session_id = String(index + 1);
    source.offline_at = source.last_observed_at;
  });
}
const regions: Record<string, string> = {
  '198.51.100.77': 'VN',
  '203.0.113.24': 'CN',
  '203.0.113.61': 'CN',
  '2001:db8:4c1a:20:5a2f:1bff:fe33:c901': 'CN',
  '203.0.113.17': 'RU',
  '2001:db8:abcd:ffff:1111:2222:3333:4444': 'NL',
};
const operators: Record<string, string> = {
  '203.0.113.24': 'chinanet',
  '203.0.113.61': 'unicom',
  '2001:db8:4c1a:20:5a2f:1bff:fe33:c901': 'cmcc',
};
const locate = (ips: string[]) => ({
  source_countries: ips.filter(ip => regions[ip]).map(ip => ({ ip, country: regions[ip] })),
  source_operators: ips.filter(ip => operators[ip]).map(ip => ({ ip, operator: operators[ip] })),
});
const bannedIps = ['203.0.113.17', '2001:db8:abcd:ffff:1111:2222:3333:4444'];
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['snapshot'], snapshot);
users.forEach(user =>
  client.setQueryData(['source-access', `/users/${tenant}/${user.id}/source-access`], {
    revision: 7,
    ips: bannedIps,
    nodes: [{ node_id: 'n1', phase: 'applied' }],
    ...locate(bannedIps),
  }),
);
client.setQueryData(['nodes'], {
  nodes: [
    { node_id: 'n1', name: '示例节点' },
    ...(incompletePresence
      ? [
          {
            node_id: 'n2',
            name: params.has('longNode') ? `edge-${'SingaporeTokyo'.repeat(10)}` : '东京入口节点',
          },
          { node_id: 'n3', name: '尚未上报节点' },
        ]
      : []),
  ],
});
client.setQueryData(['users'], { users });
client.setQueryData(['user-presence', role], {
  freshness_secs: 120,
  users: withSources
    ? [
        {
          tenant_id: tenant,
          user_id: 'alice',
          state: incompletePresence ? presenceState : 'complete',
          expected_nodes: incompletePresence ? 3 : 1,
          reporting_nodes: presenceState === 'unavailable' ? 0 : 1,
          ...(incompletePresence && {
            missing_nodes: [
              { node_id: 'n2', last_reported_at: new Date(Date.now() - 13 * MINUTE).toISOString() },
              { node_id: 'n3', last_reported_at: null },
            ],
          }),
          sources: presenceState === 'unavailable' ? [] : onlineSources,
        },
      ]
    : [],
  ...locate(onlineSources.map(source => source.ip)),
});
client.setQueryData(['tenants'], { tenants: [{ id: tenant, name: '示例' }] });
client.setQueryData(['quotas'], {
  quotas: users.flatMap(user =>
    snapshot.snapshot.apps.map(app => ({
      tenant_id: tenant,
      user_id: user.id,
      app_id: app.id,
      limit_bytes: appLimit(app.id) * GiB,
      updated_at: '',
      suspended_ingresses: [],
    })),
  ),
});
client.setQueryData(['user-cycles'], {
  periods: users.map(user => ({
    tenant_id: tenant,
    user_id: user.id,
    starts_at: '2026-09-30T16:00:00Z',
    ends_at: '2026-10-31T16:00:00Z',
  })),
  forwarding: [],
  traffic_policies: users.flatMap(user =>
    snapshot.snapshot.apps.map(app => ({
      tenant_id: tenant,
      user_id: user.id,
      kind: 'access',
      app_id: app.id,
      quota_bytes: String(appLimit(app.id) * GiB),
      used_bytes: String((app.id === 'budget' ? 12 : 121) * GiB),
      exhausted: appExhausted(app.id),
      suspended: appExhausted(app.id) && trafficState === 'stopped',
      rate_bps: app.id === 'budget' || trafficState === 'stopped' ? null : 5_000_000,
    })),
  ),
  views: users.flatMap((user, i) =>
    snapshot.snapshot.apps.map(app => ({
      tenant_id: tenant,
      user_id: user.id,
      app_id: app.id,
      uplink_bytes: GiB,
      downlink_bytes: (app.id === 'budget' ? 11 : 120 + i * 40) * GiB,
      has_gap: false,
    })),
  ),
});
if (role === 'user') client.setQueryData(['me-user'], users[0]);
const empty = { rows: [], series: [], sources: [], entitlements: [], rules: [], nodes: [] };
globalThis.fetch = async (input, init) => {
  const path = String(input);
  if (path.endsWith('/source-access') && (!init?.method || init.method === 'GET'))
    return Response.json(client.getQueryData(['source-access', path]));
  if (path.endsWith('/traffic-policy'))
    return params.get('policy') === 'error'
      ? Response.json({ error: '策略暂不可用' }, { status: 503 })
      : Response.json(snapshot.snapshot.apps.map(app => policyFor(app.id)));
  if (path.endsWith('/forwarding')) {
    if (params.get('forwarding') === 'pending') return new Promise<Response>(() => {});
    if (params.get('forwarding') === 'error') return Response.json({ error: '读取转发失败' }, { status: 503 });
    if (params.get('forwarding') === 'quota')
      return Response.json({
        tenant_id: tenant,
        user_id: selected,
        ...empty,
        entitlements: [
          {
            node_id: 'n1',
            node_name: '东京入口',
            public_ipv4: '198.51.100.10',
            enabled: true,
            tcp_allowed: true,
            udp_allowed: true,
            listen_port_start: 30000,
            listen_port_end: 59999,
            max_rules: 100,
            allow_private_targets: false,
            rule_count: 0,
            desired_generation: 1,
            applied_generation: 1,
            reconcile_status: 'converged',
            last_error: null,
            cycle_limit_bytes: 50 * GiB,
            cycle_used_bytes: 5 * GiB,
          },
        ],
      });
    return Response.json({ tenant_id: tenant, user_id: selected, ...empty });
  }
  if (path.endsWith('/presence-history'))
    return Response.json({
      tenant_id: tenant,
      user_id: selected,
      retention_days: 30,
      truncated: false,
      sources: selected === 'alice' ? historySources : [],
      ...locate(historySources.map(source => source.ip)),
    });
  if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};
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
  data: { drill: { p: 'user', id: selected } },
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: role === 'user' ? 'alice' : 'operator',
          role,
          tenant_scope: null,
          token_prefix: null,
          self_user: role === 'user' ? { tenant_id: tenant, user_id: 'alice' } : undefined,
          masked_assets: role === 'user',
        },
        initial: { node_count: 1, chain_group_count: [] },
      }}
    >
      <UsersPane win={win} />
    </SessionProvider>
  </QueryClientProvider>,
);
