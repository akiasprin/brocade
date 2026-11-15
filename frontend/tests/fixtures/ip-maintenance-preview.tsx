import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NodesPane } from '../../src/panes/nodes';
import { SessionProvider } from '../../src/session';
import { theme } from '../../src/forge/theme';
import { palette } from '../../src/forge/palette';
import type { NodeAgentStateItem, ConsoleSnapshot, IpMaintenancePolicy, IpMaintenanceReport } from '../../src/api';
import type { Win } from '../../src/wm/store';
import { installBackend, FixtureEventSource } from './console-backend';
import {
  maintenanceRegions,
  maintenanceReport,
  maintenanceView,
  maintenanceResetReport,
  maintenancePartialReport,
} from './ip-maintenance-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
const visitor = params.get('role') === 'public';
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
palette.set('dailan');
installBackend();
const baseFetch = window.fetch;
const nodeResponse = (await (await baseFetch('/nodes/agent-state')).json()) as { nodes: NodeAgentStateItem[] };
const node: NodeAgentStateItem = {
  ...nodeResponse.nodes[0],
  name: '东京 · 服务出口',
  public_ipv4: '192.0.2.25',
  public_ipv4_country: 'JP',
  agent_protocol_version: 70,
  last_poll_at: new Date().toISOString(),
  token_last_used_at: new Date().toISOString(),
};
if (params.has('country')) node.public_ipv4_country = params.get('country');
if (params.has('name')) node.name = params.get('name')!;
if (params.get('family') === 'v6') {
  node.public_ipv4 = null;
  node.public_ipv6 = '2606:4700::1111';
  node.public_ipv4_country_override = node.public_ipv4_country;
}
const snapshot = (await (await baseFetch('/model/snapshot')).json()) as ConsoleSnapshot;
const model = snapshot.snapshot.nodes?.find(item => item.id === node.node_id);
if (model) {
  model.name = node.name;
  model.public_ipv4 = node.public_ipv4;
}
const view = maintenanceView();
if (params.has('evidence')) {
  const countries = params.get('evidence')!.split(',');
  view.report!.evidence = view.report!.evidence.map((item, index) => ({
    ...item,
    country: countries[index] || null,
  }));
}
const runs: IpMaintenanceReport[] = [
  {
    ...maintenanceReport(),
    round_id: 'fixture-round-18',
    phase: 'complete' as const,
    action_index: 5,
    actions: Array.from({ length: 5 }, (_, index) => ({ ...maintenanceReport().actions[0], index: index + 1 })),
  },
  {
    ...maintenanceReport(),
    round_id: 'fixture-round-17',
    phase: 'cooldown' as const,
    module: 'trust' as const,
    reason: 'service_restricted',
  },
];
if (params.get('scene')?.startsWith('dual')) {
  node.public_ipv6 = '2001:db8::25';
  view.desired.policy.family = 'dual';
  view.family_reports = [
    {
      ...maintenanceReport(),
      family: 'v4',
      round_id: 'fixture-v4',
      evidence: maintenanceReport().evidence.map((e, i) => ({ ...e, country: i < 2 ? 'JP' : 'US' })),
    },
    {
      ...maintenanceReport(),
      family: 'v6',
      round_id: 'fixture-v6',
      sequence: 43,
      actual_exit_ip: '2001:db8::25',
      phase: 'requesting',
      daily_requests: 40,
      evidence: maintenanceReport().evidence.map((e, i) => ({ ...e, country: i === 0 ? null : i === 1 ? 'JP' : 'US' })),
    },
  ];
  if (params.get('scene') === 'dual-idle') {
    view.desired.policy.enabled = false;
    for (const report of view.family_reports) report.phase = 'complete';
  }
  view.report = view.family_reports[1];
  runs.splice(0, runs.length, ...structuredClone(view.family_reports));
}
if (['initial', 'initial-legacy'].includes(params.get('scene') ?? '')) {
  view.desired.generation = 0;
  view.desired.policy.enabled = false;
  view.desired.report_sequence_floor = 1;
  view.report = {
    ...maintenanceReport(),
    generation: 0,
    run_generation: 0,
    sequence: 1,
    phase: 'disabled',
    round_id: null,
    module: null,
    actual_exit_ip: null,
    actions: [],
    actions_total: 0,
    action_index: 0,
    evidence: [],
    phase_deadline_unix_secs: null,
    next_run_unix_secs: null,
    daily_requests: 0,
    daily_response_bytes: 0,
  };
  runs.splice(0);
}
if (params.get('scene') === 'waiting') {
  view.report!.phase = 'waiting';
  view.report!.round_id = null;
  view.report!.actions = [];
  view.report!.action_index = 0;
  view.report!.actions_total = 0;
}
if (params.get('scene') === 'requesting') {
  view.report!.phase = 'requesting';
  view.report!.actions[0].status = null;
}
if (params.get('scene') === 'offline') {
  node.last_poll_at = '2020-01-01T00:00:00Z';
  view.desired.generation = 13;
  view.desired.policy.enabled = false;
}
if (params.get('scene') === 'worker-stale') view.report!.worker_observed_at_unix_secs -= 120;
if (params.get('scene') === 'inactive') {
  view.lifecycle_active = false;
  view.desired.policy.continue_offline = true;
  node.last_poll_at = '2020-01-01T00:00:00Z';
  view.report!.worker_observed_at_unix_secs -= 120;
  view.report!.actions[0].status = null;
}
if (['legacy', 'initial-legacy'].includes(params.get('scene') ?? '')) {
  view.supported = false;
  view.dual_stack_supported = false;
  node.agent_protocol_version = 67;
  view.report = null;
  view.received_at_unix_secs = null;
}
if (params.get('scene') === 'budget') {
  view.report!.phase = 'budget_wait';
  view.report!.reason = 'daily_budget';
  runs.unshift(structuredClone(view.report!));
}
if (params.get('scene') === 'cooldown') {
  view.report!.phase = 'cooldown';
  view.report!.cooldown_until_unix_secs = Math.floor(Date.now() / 1000) + 3600;
  view.report!.reason = 'service_restricted';
}
if (params.get('scene') === 'failed') {
  view.report!.phase = 'failed';
  view.report!.reason = 'network_timeout';
  view.report!.actions = [];
}
if (params.get('scene') === 'expired-cooldown') {
  view.desired.policy.enabled = false;
  view.report!.phase = 'cooldown';
  view.report!.cooldown_until_unix_secs = Math.floor(Date.now() / 1000) - 1;
  view.report!.reason = 'service_restricted';
}
if (params.get('scene') === 'partial') {
  view.desired.policy.enabled = false;
  view.report = maintenancePartialReport();
  if (params.get('issues') === 'consent') {
    view.report.actions = view.report.actions.map((action, index) => ({
      ...action,
      status: index === 3 ? 200 : null,
      error: ['consent_required', 'javascript_required', 'empty_response', null][index],
    }));
  }
  if (params.get('issues') === 'ipv6') {
    view.desired.policy.family = 'v6';
    view.report.family = 'v6';
    view.report.actual_exit_ip = '2001:db8::25';
    view.report.module = 'trust';
    view.report.evidence = [];
    view.report.evidence_observed_at_unix_secs = null;
    view.report.actions_total = 3;
    view.report.action_index = 3;
    view.report.actions = ['www.rakuten.co.jp', 'www.yahoo.co.jp', 'www.mhlw.go.jp'].map((host, index) => ({
      index: index + 1,
      kind: 'local_site',
      host,
      error: index < 2 ? 'no_ipv6_address' : null,
      status: index < 2 ? null : 200,
      response_bytes: index < 2 ? 0 : 16 * 1024,
    }));
  }
  runs.splice(0, runs.length, structuredClone(view.report));
}
class MaintenanceEvents extends FixtureEventSource {
  static instances = new Set<MaintenanceEvents>();
  private maintenanceListeners = new Map<string, ((event: MessageEvent) => void)[]>();
  constructor(url: string) {
    super(url);
    MaintenanceEvents.instances.add(this);
    if (url.includes('/observations/')) setTimeout(() => this.signal('ready', {}), 0);
  }
  addEventListener(type: string, listener: (event: MessageEvent) => void) {
    super.addEventListener(type, listener);
    this.maintenanceListeners.set(type, [...(this.maintenanceListeners.get(type) ?? []), listener]);
  }
  signal(type: string, payload: unknown) {
    for (const listener of this.maintenanceListeners.get(type) ?? [])
      listener(new MessageEvent(type, { data: JSON.stringify(payload) }));
  }
  close() {
    super.close();
    MaintenanceEvents.instances.delete(this);
  }
}
(globalThis as { EventSource: unknown }).EventSource = MaintenanceEvents;
const notify = () => MaintenanceEvents.instances.forEach(source => source.signal('invalidate', ['ip_maintenance']));
let demoTimer = 0;
function demonstrateRound() {
  clearInterval(demoTimer);
  view.report = {
    ...maintenanceReport(),
    generation: view.desired.generation,
    run_generation: view.desired.generation,
    applied_control_sequence: view.desired.control_sequence,
    applied_stop_sequence: view.desired.stop_sequence,
    applied_session_generation: view.desired.session_generation,
    phase: 'preflight',
    round_id: `demo-${Math.floor(Date.now() / 1000)}`,
    action_index: 0,
    actions: [],
    evidence: [],
    phase_deadline_unix_secs: null,
  };
  let step = 0;
  const kinds = ['google_search', 'google_news', 'google_maps', 'platform_page', 'google_search'];
  demoTimer = window.setInterval(() => {
    const report = view.report!;
    const stamp = Math.floor(Date.now() / 1000);
    step += 1;
    report.sequence += 1;
    report.observed_at_unix_secs = stamp;
    report.worker_observed_at_unix_secs = stamp;
    view.received_at_unix_secs = stamp;
    if (step <= 10) {
      const index = Math.ceil(step / 2);
      report.action_index = index;
      if (step % 2) {
        report.phase = 'requesting';
        report.phase_deadline_unix_secs = null;
        report.daily_requests += 1;
        report.actions.push({
          index,
          kind: kinds[index - 1],
          host: index === 2 ? 'news.google.com' : index === 4 ? 'about.google' : 'www.google.com',
          status: null,
          error: null,
          response_bytes: 0,
        });
      } else {
        report.actions[index - 1].status = 200;
        report.actions[index - 1].response_bytes = 20480;
        report.daily_response_bytes += 20480;
        report.phase = step === 10 ? 'postcheck' : 'dwelling';
        report.phase_deadline_unix_secs = step === 10 ? null : stamp + 1;
      }
    } else {
      report.phase = 'complete';
      report.evidence = maintenanceReport().evidence;
      report.evidence_observed_at_unix_secs = stamp;
      report.next_run_unix_secs = stamp + view.desired.policy.interval_minutes * 60;
      runs.unshift(structuredClone(report));
      clearInterval(demoTimer);
    }
    notify();
  }, 1200);
  notify();
}
if (visitor) {
  node.public_ipv4 = '***';
  if (node.public_ipv6) node.public_ipv6 = '***';
  if (model) model.public_ipv4 = node.public_ipv4;
  if (view.desired.policy.source_ip) view.desired.policy.source_ip = '***';
  if (view.desired.policy.expected_public_ip) view.desired.policy.expected_public_ip = '***';
  for (const report of [view.report, ...runs]) {
    if (!report) continue;
    if (report.actual_exit_ip) report.actual_exit_ip = '***';
    for (const action of report.actions) action.host = '***';
  }
}
let maintenanceWrites = 0;
Object.defineProperty(window, '__maintenanceWrites', { get: () => maintenanceWrites });
window.fetch = async (input, init) => {
  const url = new URL(String(input), location.origin);
  if (url.pathname.includes('/ip-maintenance') && init?.method && init.method.toUpperCase() !== 'GET')
    maintenanceWrites++;
  if (url.pathname === '/nodes/agent-state') return Response.json({ nodes: [node] });
  if (url.pathname === '/model/snapshot')
    return params.get('scene') === 'model-error'
      ? Response.json({ error: '模型读取失败' }, { status: 503 })
      : Response.json(snapshot);
  if (url.pathname === '/ip-maintenance/regions') return Response.json(maintenanceRegions);
  if (url.pathname === '/certs')
    return Response.json({ sealing_available: true, domain: null, groups: [], nodes: [], scan: null });
  if (url.pathname === `/nodes/${node.node_id}/source-access`)
    return Response.json({
      revision: 0,
      ips: [],
      nodes: [{ node_id: node.node_id, phase: 'applied' }],
      user_bans: [],
      source_countries: [],
      source_operators: [],
    });
  if (url.pathname === `/nodes/${node.node_id}/ip-maintenance/runs`) return Response.json({ runs, next_cursor: null });
  if (url.pathname.startsWith(`/nodes/${node.node_id}/ip-maintenance/runs/`))
    return Response.json(runs.find(r => r.round_id === decodeURIComponent(url.pathname.split('/').at(-1)!)) ?? runs[0]);
  if (url.pathname === `/nodes/${node.node_id}/ip-maintenance/stop`) {
    const body = JSON.parse(String(init?.body));
    const report = (view.family_reports ?? (view.report ? [view.report] : [])).find(
      report => report.round_id === body.round_id,
    );
    if (!report) return Response.json({ error: '轮次已变化' }, { status: 409 });
    clearInterval(demoTimer);
    view.desired.control_sequence++;
    view.desired.stop_sequence = view.desired.control_sequence;
    view.desired.target_round_id = body.round_id;
    const sequence = view.desired.stop_sequence;
    if (params.get('scene') !== 'offline')
      setTimeout(() => {
        report.phase = 'interrupted';
        report.reason = 'stop_requested';
        report.applied_control_sequence = sequence;
        report.applied_stop_sequence = sequence;
        notify();
      }, 500);
    notify();
    return new Response(null, { status: 204 });
  }
  if (url.pathname === `/nodes/${node.node_id}/ip-maintenance`) {
    if (init?.method === 'PUT') {
      const body = JSON.parse(String(init.body)) as { expected_generation: number; policy: IpMaintenancePolicy };
      if (body.expected_generation !== view.desired.generation)
        return Response.json({ error: '策略版本冲突' }, { status: 409 });
      view.desired = {
        ...view.desired,
        generation: view.desired.generation + 1,
        policy: body.policy,
        control_sequence: view.desired.control_sequence + 1,
      };
      if (params.has('demo')) clearInterval(demoTimer);
      notify();
      if (params.get('scene') !== 'offline')
        setTimeout(() => {
          const activeManual =
            view.report?.run_generation === view.desired.generation &&
            ['requesting', 'dwelling', 'postcheck'].includes(view.report.phase);
          const previous = view.report!;
          view.report = {
            ...previous,
            generation: view.desired.generation,
            applied_control_sequence: view.desired.control_sequence,
            sequence: (view.report?.sequence ?? 0) + 1,
            phase: activeManual ? previous.phase : view.desired.policy.enabled ? 'waiting' : 'disabled',
            round_id: activeManual ? previous.round_id : null,
            module: activeManual ? previous.module : null,
            actions: activeManual ? previous.actions : [],
            actions_total: activeManual ? previous.actions_total : 0,
            action_index: activeManual ? previous.action_index : 0,
            phase_deadline_unix_secs: activeManual ? previous.phase_deadline_unix_secs : null,
            next_run_unix_secs: activeManual
              ? previous.next_run_unix_secs
              : view.desired.policy.enabled
                ? Math.floor(Date.now() / 1000) + view.desired.policy.interval_minutes * 60
                : null,
          };
          notify();
        }, 500);
      return new Response(null, { status: 204 });
    }
    return Response.json(structuredClone(view));
  }
  if (url.pathname === `/nodes/${node.node_id}/ip-maintenance/run`) {
    view.desired.control_sequence++;
    view.report!.applied_control_sequence = view.desired.control_sequence;
    if (params.has('demo')) {
      demonstrateRound();
      return new Response(null, { status: 204 });
    }
    view.report!.generation = view.desired.generation;
    view.report!.phase = 'requesting';
    view.report!.run_generation = view.desired.generation;
    view.report!.round_id = 'fixture-manual-round';
    view.report!.actions[0] = {
      index: 1,
      kind: 'google_search',
      host: 'www.google.com',
      status: null,
      error: null,
      response_bytes: 0,
    };
    view.report!.actions_total = 5;
    view.report!.action_index = 1;
    notify();
    return new Response(null, { status: 204 });
  }
  if (url.pathname === `/nodes/${node.node_id}/ip-maintenance/session-reset`) {
    if (params.has('resetDelay')) await new Promise(resolve => setTimeout(resolve, 150));
    view.desired.generation++;
    view.desired.control_sequence++;
    view.desired.session_generation++;
    view.desired.policy.enabled = false;
    view.report = params.has('resetDelay') ? null : maintenanceResetReport(view.desired);
    runs.splice(0);
    notify();
    if (params.has('resetDelay'))
      setTimeout(() => {
        view.report = maintenanceResetReport(view.desired);
        notify();
      }, 200);
    return new Response(null, { status: 204 });
  }
  return baseFetch(input, init);
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['nodes'], { nodes: [node] });
if (params.get('scene') !== 'model-error') client.setQueryData(['snapshot'], snapshot);
if (params.has('demo'))
  setInterval(() => {
    if (params.get('scene') !== 'inactive') node.last_poll_at = new Date().toISOString();
    node.token_last_used_at = node.last_poll_at;
    client.setQueryData(['nodes'], { nodes: [{ ...node }] });
    if (view.report && !['offline', 'worker-stale', 'inactive'].includes(params.get('scene') ?? '')) {
      const stamp = Math.floor(Date.now() / 1000);
      view.report.worker_observed_at_unix_secs = stamp;
      view.received_at_unix_secs = stamp;
      notify();
    }
  }, 30_000);
const win: Win = {
  id: 9125,
  key: 'tab:nodes',
  title: '机器',
  x: 0,
  y: 0,
  w: 1200,
  h: 1000,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'node', id: node.node_id, tab: params.has('legacyTab') ? 'care' : 'config' } },
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: visitor ? 'public' : 'preview',
          role: visitor || params.get('role') === 'readonly' ? 'readonly' : 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: visitor || params.get('role') === 'readonly',
        },
        initial: { node_count: 1, chain_group_count: [] },
      }}
    >
      <div className={`forge${innerWidth <= 820 ? ' narrow' : ''}`}>
        <main className="fg-desk" style={{ padding: 16 }}>
          {params.has('demo') && (
            <p className="note" style={{ maxWidth: 1120, margin: '0 auto 16px' }}>
              生产组件交互演示 · 使用模拟 Agent
              回执，未连接真实节点。点击「立即执行一轮」查看加速进度，演示不会发出公网养护请求。
            </p>
          )}
          <NodesPane win={win} bare />
        </main>
      </div>
    </SessionProvider>
  </QueryClientProvider>,
);
