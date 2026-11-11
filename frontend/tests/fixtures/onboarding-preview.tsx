// Mount the production console on a fresh installation, frozen at one first-run phase.
// Query: phase=machine|install|chain|user|publish|activate|failed|verify|complete, theme=light|dark, initial=complete.
// Synthetic data only; the onboarding checklist and every page behind it are production components.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { CVM_ENABLED } from '../../src/product';
import { theme } from '../../src/forge/theme';
import type {
  DeploymentDetail,
  DeploymentListItem,
  NodeAgentStateItem,
  OnboardingState,
  RevisionList,
} from '../../src/api';
import { draft } from '../../src/draft';
import { installBackend } from './console-backend';
import { SETTINGS } from './console-model-data';
import { settings as unlockSettings } from './unlock-data';
import '../../src/styles.css';

const PHASES = ['machine', 'install', 'chain', 'user', 'publish', 'activate', 'failed', 'verify', 'complete'] as const;
type Phase = (typeof PHASES)[number];
const params = new URLSearchParams(location.search);
const phase: Phase = (PHASES as readonly string[]).includes(params.get('phase') ?? '')
  ? (params.get('phase') as Phase)
  : 'chain';
const at = (step: Phase) => PHASES.indexOf(phase) >= PHASES.indexOf(step);
const registered = at('install');
const online = at('chain');
const committed = at('publish');
const deployed = at('activate');
const serving = at('verify');

const NOW = Date.now();
const iso = (ms: number) => new Date(ms).toISOString();
const STARTED = NOW - 26 * 60_000;
const NODE = 'hk-01';
const CHAIN = 'hk-01-direct';
const APP = 'default';

const machine = {
  node_id: NODE,
  tenant_id: 'platform',
  name: '香港入口',
  public_ipv4: online ? '203.0.113.10' : null,
  public_ipv4_country: online ? 'HK' : null,
  public_ipv6: null,
  public_ipv4_nat: false,
  public_ipv6_nat: false,
  route_ipv4: null,
  route_ipv6: null,
  retired_at: null,
  lifecycle_phase: 'active',
  agent_protocol_version: online ? 67 : null,
  runtime_report_fresh: online,
  runtime_versions: online ? { openvpn: null } : null,
  token_prefix: 'brc_enr_7f3a',
  token_last_used_at: online ? iso(STARTED + 9 * 60_000) : null,
  last_poll_at: online ? iso(NOW - 3_000) : null,
  initialization: {
    state: online ? 'succeeded' : 'waiting-agent',
    revision_id: 2,
    deployment_id: online ? 1 : null,
    reason: null,
  },
} as unknown as NodeAgentStateItem;
const machines = registered ? [machine] : [];

let onboarding: OnboardingState = {
  status: phase === 'complete' ? 'complete' : 'active',
  root_tenant_id: 'platform',
  started_at: iso(STARTED),
  node_id: registered ? NODE : null,
  app_id: committed ? APP : null,
  chain_id: committed ? CHAIN : null,
  ingress_id: committed ? CHAIN : null,
  user_tenant_id: committed ? 'platform' : null,
  user_id: committed ? 'alice' : null,
  target_revision_id: committed ? 3 : null,
  deployment_id: deployed ? 2 : null,
  completed_at: phase === 'complete' ? iso(NOW - 30_000) : null,
};

const grantProbeItem = {
  id: 'hk-01-direct:ipv4:vless',
  name: '香港直出',
  app_id: APP,
  app_name: '默认分组',
  chain_id: CHAIN,
  ingress_id: CHAIN,
  family: 'ipv4',
  protocol: 'vless',
} as const;

const snapshot = {
  snapshot: {
    revision: committed ? 3 : registered ? 2 : 1,
    nodes: machines.map(node => ({
      id: node.node_id,
      certificate_name: null,
      certificate_track: null,
      egress_allowed: true,
    })),
    apps: committed
      ? [
          {
            id: APP,
            label: '默认分组',
            chains: [{ id: CHAIN, tenant: 'platform', name: '香港直出' }],
            ingresses: [
              { id: CHAIN, chain: CHAIN, node: NODE, port: 13443, wires: { vless: { kind: 'vless-reality' } } },
            ],
            steps: [
              {
                chain: CHAIN,
                node: NODE,
                accept: null,
                hop_in: null,
                rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
              },
            ],
            fronts: [],
            grants: [{ tenant: 'platform', user: 'alice', ingress: CHAIN }],
          },
        ]
      : [],
    external_outbounds: [],
  },
  node_egress_dns: [],
  redacted: false,
};

const revisions = {
  current_revision: committed ? 3 : registered ? 2 : 1,
  revisions: [
    ...(committed ? [{ id: 3, note: '首次上线：创建第一条链与第一个用户' }] : []),
    ...(registered ? [{ id: 2, note: `纳管 ${NODE}` }] : []),
    { id: 1, note: '初始化控制台' },
  ].map((row, index) => ({
    ...row,
    created_at: iso(STARTED + (3 - row.id) * 6 * 60_000),
    author: 'admin',
    status: 'published',
    current: index === 0,
    has_snapshot: true,
  })),
} as unknown as RevisionList;

const failed = phase === 'failed';
const deployment = {
  id: 2,
  revision_id: 3,
  status: failed ? 'failed' : serving ? 'succeeded' : 'running',
  activation_status: serving ? 'activated' : 'waiting',
  settlement_status: 'converged',
  activated_at: serving ? iso(NOW - 60_000) : null,
  active: !serving && !failed,
  actor: 'admin',
  kind: 'config',
  note: null,
  base_revision_id: 2,
  rollback_of_deployment_id: null,
  sync_of_deployment_id: null,
  created_at: iso(NOW - 90_000),
  started_at: iso(NOW - 90_000),
  finished_at: serving || failed ? iso(NOW - 60_000) : null,
  total_targets: 1,
  changed_targets: 1,
  skipped_targets: 0,
  failed_targets: failed ? 1 : 0,
  debt_targets: 0,
  disruptive_targets: 0,
  max_wave: 1,
  awaiting_confirmation: false,
} as DeploymentListItem;
const detail = {
  ...deployment,
  warnings: [],
  halted_at: null,
  divergence_cleared_at: null,
  wave_progress: [{ wave: 1, entered_at: deployment.created_at }],
  targets: [
    {
      node_id: NODE,
      status: failed ? 'failed-dirty' : serving ? 'succeeded' : 'converging',
      error: failed ? 'xray 配置校验失败：端口 13443 已被占用' : null,
      wave: 1,
      disruptive: false,
      desired_structure: null,
      observed_before: null,
      observed_after: null,
      verdict: null,
      dispatched_at: deployment.started_at,
    },
  ],
} as unknown as DeploymentDetail;

const EMPTY: Record<string, unknown> = {
  '/notifications': {
    retention_days: 14,
    latest_event_id: 0,
    last_seen_event_id: 0,
    cleared_through_event_id: 0,
    unread_count: 0,
    active: [],
    events: [],
  },
  '/node-traffic': { nodes: [] },
  '/load/nodes/nic': { nodes: [] },
  '/load/nodes': { nodes: [] },
  '/ping-probe/nodes/latest': { nodes: [] },
  '/probes/e2e': { chains: [] },
  '/links/health': { hops: [] },
  '/users/presence': { users: [] },
  '/quotas': { quotas: [] },
  '/usage/user-cycles': { periods: [], views: [] },
  '/realtime/fleet-traffic/snapshot': { policy: { enabled: true }, nodes: [], expected: [] },
};

/** A fresh fleet has no traffic samples; only a connected first machine appears on the realtime channel. */
class FirstRunEventSource {
  onerror: (() => void) | null = null;
  onopen: (() => void) | null = null;
  private listeners = new Map<string, ((event: MessageEvent) => void)[]>();
  constructor(readonly url: string) {
    const path = new URL(url, location.origin).pathname;
    setTimeout(() => {
      if (path !== '/realtime/nodes/events' && path !== '/realtime/fleet-traffic/events') return;
      const nodes = online ? [{ node_id: NODE, connected: true, active: true, interval_secs: 1, samples: [] }] : [];
      this.emit('snapshot', path === '/realtime/nodes/events' ? { nodes } : { policy: { enabled: true }, nodes });
    }, 30);
  }
  addEventListener(type: string, listener: (event: MessageEvent) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }
  removeEventListener(type: string, listener: (event: MessageEvent) => void) {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter(item => item !== listener),
    );
  }
  close() {}
  private emit(type: string, data: unknown) {
    const event = new MessageEvent(type, { data: JSON.stringify(data) });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

// 「用户」一步的线路只在浏览器草稿里：建链页「加入草稿」写入的分组、链与入口。
if (phase === 'user') {
  draft.init('admin');
  draft.push({ op: 'upsert_app', app: { id: APP, label: '默认分组' } });
  draft.push({ op: 'create_chain', app_id: APP, chain: { id: CHAIN, tenant_id: 'platform', name: '香港直出' } });
  draft.push({
    op: 'create_ingress',
    app_id: APP,
    ingress: {
      id: CHAIN,
      chain_id: CHAIN,
      node_id: NODE,
      bind: '',
      port: 13443,
      reality: { fallback_mode: 'global-site', fallback_guard: true },
      wires: { vless: { kind: 'vless-reality' } },
      projection: {},
      guard: { no_private: true, no_bittorrent: true },
    },
  } as never);
}

if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
installBackend();
(globalThis as { EventSource: unknown }).EventSource = FirstRunEventSource;
const fleet = globalThis.fetch;
const json = (body: unknown) => Response.json(body);
globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const path = url.pathname;
  if (path === '/bootstrap')
    return json({
      features: { cvm: CVM_ENABLED },
      who: { operator_id: 'admin', role: 'system-admin', tenant_scope: null, token_prefix: null, masked_assets: false },
      initial: {
        node_count: machines.length,
        chain_group_count: committed ? [[APP, 1]] : [],
        // 「刚刚完成」：本次会话里走完最后一步，清单仍在。
        onboarding: params.get('initial') === 'complete' ? 'complete' : 'active',
      },
    });
  if (path === '/onboarding') return json(onboarding);
  if (path === '/onboarding/complete' && init?.method === 'POST') {
    onboarding = { ...onboarding, status: 'complete', completed_at: iso(NOW) };
    return json(onboarding);
  }
  if (path === '/nodes/agent-state') return json({ nodes: machines });
  if (path === '/model/snapshot') return json(snapshot);
  if (path === '/model/preview')
    return json({
      snapshot,
      compile: { diagnostics: [], system: { nodes: [] } },
      artifacts: { revision: 0, artifacts: [] },
    });
  if (path === '/users')
    return json({
      users: committed
        ? [{ tenant_id: 'platform', id: 'alice', status: 'active', created_at: iso(STARTED), created_revision: 3 }]
        : [],
    });
  if (path === '/revisions') return json(revisions);
  if (path === '/tenants')
    return json({
      tenants: [{ id: 'platform', name: '平台', node_count: machines.length, user_count: 0, operator_count: 1 }],
    });
  if (path === '/settings')
    return json({
      ...unlockSettings,
      ...SETTINGS,
      reality_site: { ...unlockSettings.reality_site, ...SETTINGS.reality_site },
    });
  if (path === '/certs')
    return json({
      sealing_available: true,
      domain: null,
      groups: [],
      nodes: [],
      scan: null,
      letsencrypt: 'https://acme.example.test/directory',
      letsencrypt_staging: 'https://staging.acme.example.test/directory',
    });
  if (/^\/compile\/\d+$/.test(path))
    return json({
      diagnostics: [],
      system: { nodes: machines.map(node => ({ id: node.node_id, wg_listen_port: 51820 })) },
    });
  if (path === '/deployments')
    return json({
      deployments: deployed ? [deployment].filter(row => !url.searchParams.has('active_only') || row.active) : [],
    });
  if (path === '/deployments/2') return json(detail);
  if (path === '/deployment-settings')
    return json({
      unattended: true,
      xray_handoff_beta: false,
      unlock_detection_mode: 'scheduled',
      unlock_detection_interval_hours: 24,
    });
  if (path === '/deployments/plan')
    return json({
      revision: 3,
      targets: [
        { node_id: NODE, status: 'pending', wave: 1, disruptive: false, actions: ['apply-xray', 'sync-grants'] },
      ],
      summary: {
        total_targets: 1,
        changed_targets: 1,
        skipped_targets: 0,
        deferred_targets: 0,
        disruptive_targets: 0,
        max_wave: 1,
      },
      warnings: [],
      base_revision_id: null,
    });
  if (path === '/deployments/plan/artifacts') return json({ revision: 3, targets: [] });
  if (path === '/artifacts/index') return json({ revision: Number(url.searchParams.get('revision')), artifacts: [] });
  if (path === '/grant-probes/capability')
    return json({ available: true, version: '26.7.28', reason: null, concurrency: 4 });
  if (path === '/users/platform/alice/grant-probes')
    return json(
      init?.method === 'POST'
        ? {
            reused: false,
            job: {
              id: 'onboarding-probe',
              kind: 'grant',
              tenant_id: 'platform',
              user_id: 'alice',
              serving_revision: 3,
              serving_generation: 7,
              status: 'completed',
              message: null,
              created_at_unix_secs: NOW / 1_000,
              finished_at_unix_secs: NOW / 1_000,
              items: [{ ...grantProbeItem, status: 'passed', ttfb_ms: 180, detail: null }],
            },
          }
        : { serving_revision: 3, serving_generation: 7, items: [grantProbeItem] },
    );
  if (path in EMPTY) return json(EMPTY[path]);
  return fleet(input, init);
};

createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <App />
  </QueryClientProvider>,
);
