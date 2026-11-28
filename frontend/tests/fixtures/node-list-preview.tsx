// Production App, shell, routing, roster and detail backed by a synthetic fleet.
// ?scene=typical|calm|trouble|few|many|empty, role=system-admin|readonly|public, theme=dark|light.
// Retirement states are opt-in so the default scenes keep their baselines:
// routes=on puts chains through hk-hkix, hk-old and kr-old; teardown=running serves the retiring
// machines' teardown deployment #58; abandoned=<id>,… marks retired machines as force-retired;
// removal=<id>:queued|failed,… schedules removal or reports a failed one.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { AdminRole, ConsoleSnapshot, DeploymentListItem, NodeAgentStateItem, Whoami } from '../../src/api';
import { DEFAULT_HOP_MUX } from '../../src/api';
import { appIdFromBytes, modelIdPairFromBytes } from '../../src/model-id';
import { App } from '../../src/app';
import { CVM_ENABLED } from '../../src/product';
import { theme } from '../../src/forge/theme';
import type { Session } from '../../src/session';
import { baremetalWorkspace, cvmWorkspace } from './cvm/data';
import {
  loadOverview,
  loadListView,
  usageSeries,
  pingSeries,
  SCENES,
  agentState,
  nicView,
  pingLatest,
  reports,
  trafficItem,
  type Machine,
  type SceneKey,
} from './node-roster-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
document.documentElement.dataset.palette = 'dailan';
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

const role = pick(params.get('role'), ['system-admin', 'readonly', 'public'] as const, 'system-admin');
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
const sceneKey = pick(params.get('scene'), Object.keys(SCENES) as SceneKey[], 'typical');
let machines: Machine[] = SCENES[sceneKey].machines();
const session: Session = { who, initial: { node_count: machines.length, chain_group_count: [] } };
const nowSecs = () => Math.floor(Date.now() / 1000);
const masked = (ip: string | null) =>
  !who.masked_assets || !ip
    ? ip
    : ip.includes(':')
      ? `${ip.split(':').slice(0, 2).join(':')}:***`
      : ip.replace(/\.\d+\.\d+$/, '.***.***');

const xrayEnabled = params.get('xray') !== 'off';

/** Chains whose every hop is the listed machines; removing a machine removes its chains. */
const ROUTES = [
  { name: '香港直连', nodes: ['hk-hkix'] },
  { name: '香港 → 东京', nodes: ['hk-hkix', 'jp-iij'] },
  { name: '旧香港入口', nodes: ['hk-old'] },
  { name: '首尔备用', nodes: ['kr-old'] },
].map((route, index) => ({ ...route, ...modelIdPairFromBytes(new Uint8Array([9, index + 1, 0, index + 1])) }));
let routes = params.get('routes') === 'on' ? ROUTES : [];
const routeApp = () => ({
  id: appIdFromBytes(new Uint8Array([9, 9])),
  label: '退役示例',
  chains: routes.map(route => ({ id: route.chainId, tenant: 'platform', name: route.name })),
  ingresses: routes.map((route, index) => ({
    id: route.ingressId,
    chain: route.chainId,
    node: route.nodes[0],
    bind: '0.0.0.0',
    port: 63001 + index,
    projection: {},
    identity: { public_key: 'preview-key', short_ids: ['0123456789abcdef'] },
    guard: {
      no_private: true,
      no_bittorrent: true,
      no_mail: true,
      no_udp_amplification: true,
      tcp_and_quic_only: false,
      cn_source_block: 'off',
    },
    wires: { vless: { kind: 'vless-reality' } },
  })),
  steps: routes.flatMap(route =>
    route.nodes.map((node, hop) => ({
      chain: route.chainId,
      node,
      accept: null,
      hop_in: hop ? { port: 22000 + hop, security: { t: 'none' } } : null,
      rules: [
        {
          m: { t: 'any' },
          a:
            hop === route.nodes.length - 1
              ? { t: 'egress', send_through: null }
              : {
                  t: 'forward',
                  to: route.nodes[hop + 1],
                  dial: { t: 'overlay' },
                  pool: { t: 'mux', v: DEFAULT_HOP_MUX },
                },
        },
      ],
    })),
  ),
  fronts: [],
  grants: [],
});

const listParam = (name: string) => (params.get(name) ?? '').split(',').filter(Boolean);
const abandoned = new Set(listParam('abandoned'));
const removals = new Map(
  listParam('removal').flatMap(entry => {
    const [id, status] = entry.split(':');
    return status === 'queued' || status === 'failed' ? [[id, status] as const] : [];
  }),
);
const REMOVAL_ERROR = '机器仍被配置发布 #61 引用';
const withLifecycle = (state: NodeAgentStateItem): NodeAgentStateItem => {
  const removal = removals.get(state.node_id) ?? null;
  const forced = abandoned.has(state.node_id) && state.lifecycle_phase !== 'active';
  return {
    ...state,
    ...(forced && {
      lifecycle_phase: 'abandoned',
      lifecycle_deployment_id: null,
      lifecycle_completed_at: state.lifecycle_completed_at ?? '2026-10-25T11:50:00Z',
      retired_at: state.retired_at ?? '2026-10-25T11:50:00Z',
    }),
    removal_status: removal,
    removal_requested_at: removal ? '2026-10-25T11:40:00Z' : null,
    removal_last_error: removal === 'failed' ? REMOVAL_ERROR : null,
  };
};
const teardown = (): DeploymentListItem[] =>
  params.get('teardown') === 'running' && machines.some(machine => machine.state === 'retiring')
    ? [
        {
          id: 58,
          revision_id: 42,
          status: 'running',
          activation_status: 'waiting',
          settlement_status: 'uncertain',
          activated_at: null,
          active: true,
          actor: 'operator',
          kind: 'config',
          note: null,
          base_revision_id: 41,
          rollback_of_deployment_id: null,
          sync_of_deployment_id: null,
          created_at: '2026-10-02T03:00:00Z',
          started_at: '2026-10-02T03:00:02Z',
          finished_at: null,
          total_targets: 1,
          changed_targets: 1,
          skipped_targets: 0,
          failed_targets: 0,
          debt_targets: 0,
          disruptive_targets: 1,
          max_wave: 1,
          awaiting_confirmation: false,
        },
      ]
    : [];
const retiredStates = new Set(['retiring', 'retired', 'cleanup']);
const updateMachine = (id: string, change: Partial<Machine>) => {
  machines = machines.map(machine => (machine.id === id ? { ...machine, ...change } : machine));
};

const snapshot = (): ConsoleSnapshot =>
  ({
    snapshot: {
      revision: 41,
      apps: routes.length ? [routeApp()] : [],
      nodes: machines.map(machine => ({
        id: machine.id,
        tenant: 'platform',
        name: machine.name,
        overlay: true,
        xray_enabled: xrayEnabled,
      })),
    },
    node_egress_dns: [],
    redacted: who.masked_assets,
  }) as unknown as ConsoleSnapshot;

const machineOf = (id: string) => machines.find(machine => machine.id === decodeURIComponent(id));
const numberParam = (url: URL, name: string) => Number(url.searchParams.get(name));
const rangeOf = (url: URL) => {
  const end = url.searchParams.has('end_unix_secs') ? numberParam(url, 'end_unix_secs') : nowSecs();
  const start = url.searchParams.has('start_unix_secs')
    ? numberParam(url, 'start_unix_secs')
    : end - (numberParam(url, 'window_secs') || 3600);
  return { start, end };
};

const requestLog: string[] = [];
Object.defineProperty(window, '__nodePreviewRequests', { value: requestLog });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input instanceof Request ? input.url : input), location.origin);
  const path = url.pathname;
  if (path === '/cvm' && (!init?.method || init.method === 'GET')) {
    const cube = params.get('cube');
    const workspace = cube === 'combined' ? baremetalWorkspace('baremetal-hosts', role) : cvmWorkspace('list', role);
    if (cube === 'pvm') workspace.hosts[0].capabilities.backend = 'cube-pvm-v1';
    if (cube === 'error' || cube === 'unmanaged') {
      workspace.hosts[0].error = 'CVM Rust executor configuration is unavailable';
      workspace.hosts[0].fresh = false;
      if (cube === 'unmanaged') {
        workspace.hosts[0].capabilities.backend = '';
        workspace.instances = [];
      }
    }
    return Response.json(workspace);
  }
  requestLog.push(path + url.search);
  const method = (init?.method ?? 'GET').toUpperCase();
  const now = nowSecs();
  const json = (body: unknown) => Response.json(body);
  if (method === 'PUT' && path.endsWith('/status')) {
    const id = decodeURIComponent(path.split('/')[2]);
    const { status } = JSON.parse(String(init?.body)) as { status: 'active' | 'retired' };
    // Restoring also cancels a scheduled removal, as the Console does.
    if (status === 'active') {
      abandoned.delete(id);
      removals.delete(id);
    }
    updateMachine(id, { state: status === 'active' ? 'ok' : 'retiring' });
    return json({ revision_id: 42 });
  }
  const lifecycle = path.match(/^\/nodes\/([^/]+)\/lifecycle\/(abandon|removal)$/);
  if (method === 'POST' && lifecycle) {
    const id = decodeURIComponent(lifecycle[1]);
    const machine = machineOf(id);
    if (lifecycle[2] === 'abandon') {
      abandoned.add(id);
      updateMachine(id, { state: 'retired' });
    } else {
      removals.set(id, 'queued');
      if (machine && !retiredStates.has(machine.state ?? 'ok')) updateMachine(id, { state: 'retiring' });
    }
    return json({ revision_id: 42 });
  }
  if (method === 'DELETE' && path === '/nodes') {
    const { node_ids } = JSON.parse(String(init?.body)) as { node_ids: string[] };
    machines = machines.filter(machine => !node_ids.includes(machine.id));
    const removedChains = routes.filter(route => route.nodes.some(node => node_ids.includes(node)));
    routes = routes.filter(route => !removedChains.includes(route));
    return json({
      revision_id: 42,
      removed_nodes: node_ids,
      removed_chains: removedChains.map(route => route.chainId),
    });
  }
  if (path === '/branding') return json({ site_name: 'Brocade', icon_data_url: null });
  if (path === '/auth/state') return json({ initialized: true, public_open: role === 'public' });
  if (path === '/bootstrap') return json({ features: { cvm: CVM_ENABLED }, ...session });
  if (path === '/whoami') return json(who);
  if (path === '/revisions')
    return json({
      current_revision: 41,
      revisions: [41, 40, 39].map(id => ({
        id,
        created_at: '2026-10-23T10:00:00Z',
        author: 'operator',
        note: null,
        status: 'committed',
        current: id === 41,
        has_snapshot: true,
      })),
    });
  if (path.startsWith('/compile/'))
    return json({
      revision: 41,
      summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
      diagnostics: [],
      system: null,
      apps: null,
      redacted: false,
    });
  if (path === '/deployments/verify' && method === 'POST')
    return json({
      revision_id: 41,
      converged: true,
      summary: {
        total_targets: machines.length,
        changed_targets: 0,
        skipped_targets: 0,
        deferred_targets: 0,
        disruptive_targets: 0,
        max_wave: 0,
      },
      targets: [],
    });
  if (path === '/deployments') return json({ deployments: teardown() });
  if (path === '/grants/automation')
    return json({
      pending_jobs: 0,
      retrying_jobs: 0,
      failed_jobs: 0,
      max_attempts: 5,
      latest_revision_id: 41,
      oldest_pending_at: null,
      last_attempt_at: null,
      last_error: null,
    });
  if (path === '/notifications')
    return json({
      retention_days: 30,
      latest_event_id: 0,
      last_seen_event_id: 0,
      cleared_through_event_id: 0,
      unread_count: 0,
      active: [],
      events: [],
    });
  if (path === '/certs')
    return json({
      sealing_available: false,
      domain: null,
      groups: [],
      nodes: [],
      scan: null,
      letsencrypt: '',
      letsencrypt_staging: '',
    });
  if (/^\/nodes\/[^/]+\/source-access$/.test(path)) return json({ revision: 1, ips: [], nodes: [], user_bans: [] });
  if (path === '/model/snapshot') return json(snapshot());
  if (path === '/nodes/agent-state')
    return json({
      nodes: machines.map(machine => {
        const state = withLifecycle(agentState(machine, now));
        return {
          ...state,
          xray_enabled: xrayEnabled,
          applied: xrayEnabled ? state.applied : { ...state.applied, xray: { state: 'disabled' } },
          public_ipv4: masked(state.public_ipv4),
          public_ipv6: masked(state.public_ipv6),
        };
      }),
    });
  if (
    params.get('metrics') === 'failed' &&
    ['/node-traffic', '/load/nodes/nic', '/load/nodes', '/ping-probe/nodes/latest'].includes(path)
  )
    return Response.json({ error: '指标暂时不可用' }, { status: 503 });
  if (path === '/node-traffic') return json({ nodes: machines.map(machine => trafficItem(machine, now)) });
  if (path === '/load/nodes/nic')
    return json({
      nodes: machines.filter(reports).map(machine => {
        const view = nicView(
          machine,
          now - (params.get('metrics') === 'stale' ? 180 : 0),
          numberParam(url, 'windows') || 24,
        );
        if (params.get('metrics') === 'gap' && view.series.length) view.series[view.series.length - 1].has_gap = true;
        if (params.get('metrics') === 'zero')
          view.series = view.series.map(sample => ({ ...sample, nic_rx_bps: 0, nic_tx_bps: 0 }));
        return view;
      }),
    });
  if (path === '/load/nodes')
    return json({
      nodes: machines.filter(reports).map(machine => {
        const view = loadListView(
          machine,
          now - (params.get('metrics') === 'stale' ? 180 : 0),
          numberParam(url, 'windows') || 6,
        );
        if (params.get('metrics') === 'gap' && view.series.length) view.series[view.series.length - 1].has_gap = true;
        return view;
      }),
    });
  if (path === '/ping-probe/nodes/latest')
    return json({ interval_secs: 10, nodes: machines.flatMap(machine => pingLatest(machine, now) ?? []) });
  if (path === '/links/mtu') return json({ links: [], nodes: [], default_mtu: 1420 });
  if (path === '/agent-log-policy') return json({ global: { max_mib: 64 }, nodes: [] });
  if (path === '/tenants') return json({ tenants: [{ id: 'platform', name: '平台' }] });
  const detail = path.match(/^\/load\/nodes\/([^/]+)\/overview$/);
  if (detail) {
    const machine = machineOf(detail[1]);
    const { start, end } = rangeOf(url);
    if (machine) return json(loadOverview(machine, start, end));
  }
  if (path === '/usage/node-series') {
    const machine = machineOf(url.searchParams.get('node_id') ?? '');
    const { start, end } = rangeOf(url);
    if (machine) return json(usageSeries(machine, start, end));
    return json({ since: new Date(start * 1000).toISOString(), month_start: new Date().toISOString(), nodes: [] });
  }
  const ping = path.match(/^\/ping-probe\/nodes\/([^/]+)\/series$/);
  if (ping) {
    const machine = machineOf(ping[1]);
    const { start, end } = rangeOf(url);
    if (machine) return json(pingSeries(machine, start, end));
  }
  const history = path.match(/^\/nodes\/([^/]+)\/public-ip-history$/);
  if (history)
    return json({ node_id: decodeURIComponent(history[1]), visible_days: 14, retention_days: 90, events: [] });
  if (path.startsWith('/artifacts/content/'))
    return json({
      revision: 41,
      target_kind: 'node',
      target_id: '',
      artifact_kind: 'xray',
      state: 'present',
      sha256: null,
      byte_len: null,
      content: JSON.stringify({}),
      redacted: false,
    });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

/** The realtime channels stay silent; every reading falls back to the polled queries. */
class SilentEventSource extends EventTarget {
  static sources: SilentEventSource[] = [];
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  readonly readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  constructor(readonly url: string) {
    super();
    SilentEventSource.sources.push(this);
  }
  close() {
    SilentEventSource.sources = SilentEventSource.sources.filter(source => source !== this);
  }
}
Object.defineProperty(window, 'EventSource', { value: SilentEventSource, configurable: true });
Object.defineProperty(window, '__nodePreviewPush', {
  value: (url: string, event: string, data: unknown) => {
    for (const source of SilentEventSource.sources) {
      if (source.url === url) source.dispatchEvent(new MessageEvent(event, { data: JSON.stringify(data) }));
    }
  },
});

if (!location.hash) history.replaceState(null, '', `${location.search}#/nodes`);
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <App />
  </QueryClientProvider>,
);
