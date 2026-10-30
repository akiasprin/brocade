// Production chain list and detail, served by a synthetic Console.
// scene=typical|calm, role=system-admin|readonly|public, theme=dark|light,
// pages=1 exercises pagination, error=1 exercises recovery, view=cards starts in the card view.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type {
  AdminRole,
  ConsoleSnapshot,
  Hysteria2Settings,
  NodeUnlockProbe,
  UnlockProbeSelection,
  Whoami,
} from '../../src/api';
import type { ModelOp } from '../../src/draft';
import { App } from '../../src/app';
import { type Session } from '../../src/session';
import { MACHINES, TENANT, chainSnapshot, e2eProbes, previewSnapshot } from './list-head-mockup-data';
import { agentState, linkHealth, nicView, pingLatest, reports, trafficItem } from './node-list-mockup-data';
import { settings } from './unlock-mockup-data';
import { SCENES, nowSecs, unlockCapabilities, unlockJob, type SceneKey } from './chain-unlock-mockup-data';
import { youtubeSplitResult } from './youtube-unlock-data';
import { withDialFailure } from './unlock-dial-data';
import '../../src/styles.css';
import '../../src/panes/node-unlock.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

/* ══ 合成后端 ══ */

const scene = pick<SceneKey>(params.get('scene'), Object.keys(SCENES) as SceneKey[], 'typical');
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
const session: Session = { who, initial: { node_count: MACHINES.length, chain_group_count: [] } };
const masked = (ip: string | null) =>
  !who.masked_assets || !ip
    ? ip
    : ip.includes(':')
      ? `${ip.split(':').slice(0, 2).join(':')}:***`
      : ip.replace(/\.\d+\.\d+$/, '.***.***');
const numberParam = (url: URL, name: string) => Number(url.searchParams.get(name));

/** The list mockup's Hysteria 2 wires predate bandwidth and masquerade; 链详情 reads both. */
function withDetailFields(view: ConsoleSnapshot): ConsoleSnapshot {
  for (const app of view.snapshot.apps)
    for (const ingress of app.ingresses) {
      const hy2 = ingress.wires.hysteria2 as Partial<Hysteria2Settings> | null | undefined;
      if (hy2)
        Object.assign(hy2, { bandwidth: hy2.bandwidth ?? {}, masquerade: hy2.masquerade ?? { kind: 'not-found' } });
    }
  return view;
}
const COMPILE_VIEW = {
  revision: 41,
  summary: { errors: 0, warnings: 0, infos: 0, can_publish: true },
  diagnostics: [],
  system: null,
  apps: null,
  redacted: false,
};

const client = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 5_000, refetchOnWindowFocus: false, refetchOnReconnect: false, retry: false },
  },
});

/** Jobs as the Console holds them, created on first read so their times are relative to page load. */
const jobs = new Map<string, NodeUnlockProbe | null>();
const capabilitiesOf = (chain: string) =>
  params.has('family-unavailable')
    ? { family_mode: 'split' as const, automatic_ready: false, ipv6_available: false, ipv6_reason: '机器无 IPv6 地址' }
    : chain === 'us-sjc'
      ? { family_mode: 'automatic' as const, automatic_ready: true, ipv6_available: null, ipv6_reason: null }
      : chain === 'tw-hinet'
        ? { family_mode: 'split' as const, automatic_ready: false, ipv6_available: true, ipv6_reason: null }
        : unlockCapabilities(chain);
const jobOf = (chain: string, selection: UnlockProbeSelection) => {
  const key = `${chain}/${selection}`;
  if (!jobs.has(key)) {
    let job = unlockJob(chain, selection === 'auto' ? 'v4' : selection, scene, nowSecs());
    if (selection === 'auto' && job) job = { ...job, family: null, automatic: true };
    if (params.has('family-unavailable') && selection === 'v6' && job)
      job = {
        ...job,
        path_error: 'family_unavailable',
        results: [],
        previous_results: job.results,
        previous_completed_at: job.completed_at,
        next_run_at: nowSecs() + 60,
      };
    if (selection === 'v6' && chain === 'tw-hinet') {
      const original = unlockJob(chain, 'v4', 'calm', nowSecs());
      if (original)
        job = {
          ...original,
          family: 'v6',
          id: `${original.id}-v6`,
          results: original.results.map(result => ({
            ...result,
            ...(result.service === 'chatgpt'
              ? { status: 'skipped' as const, code: 'no_ipv6_dns' as const, region: null }
              : {}),
            ...(result.service === 'netflix' ? { region: 'JP' } : {}),
          })),
        };
    }
    if (selection === 'v6' && chain === 'jp-iij' && job)
      job = {
        ...job,
        results: job.results.map(result => (result.service === 'gemini' ? { ...result, region: 'USA' } : result)),
      };
    if (params.has('dns-fallback') && job && selection === 'v6') {
      job.results = job.results.map(result =>
        ['chatgpt', 'claude', 'gemini'].includes(result.service)
          ? {
              ...result,
              dns_path: {
                exit: 'exit-hk',
                normal_family: 'v4',
                fallback: 'machine',
                rules: [
                  {
                    selector: { t: 'geosite', v: ['category-ai-!cn'] },
                    resolution: {
                      address: '198.51.100.53',
                      port: 53,
                      transport: 'tcp',
                      address_strategy: 'use_ip',
                      fallback: 'machine',
                    },
                  },
                ],
              },
            }
          : result,
      );
      for (const result of job.results)
        if (result.dns_path)
          result.dns_parts = [
            { part: 'service', kind: 'fallback', paths: [result.dns_path], exits: [result.dns_path.exit] },
          ];
    }
    if (params.has('premium-region-notice') && job && ['hk-direct', 'jp-iij'].includes(chain)) {
      const region = chain === 'hk-direct' ? 'CN' : 'MO';
      const result = structuredClone(youtubeSplitResult);
      result.region = region;
      result.youtube!.premium.privacy_region = region;
      result.youtube!.webpage.privacy_region = region;
      if (selection === 'v6') {
        result.status = 'failed';
        result.code = 'network_error';
        result.region = null;
        const failure = {
          status: 'failed' as const, code: 'network_error' as const, http_status: null,
          page_region: null, topbar_region: null, privacy_region: null,
        };
        result.youtube!.webpage = { ...failure };
        result.youtube!.premium = { ...failure };
        result.youtube!.cdn = {
          http_status: null, node: null, airport: null, country: null, error: 'network_error', elapsed_ms: 0,
        };
      }
      job = {
        ...job, state: 'complete', path_error: null, previous_results: [],
        results: job.results.map(original => original.service === 'youtube_premium' ? result : original),
      };
    }
    if (job && selection === 'v6' && chain === 'hk-direct' && params.has('dial-error')) {
      const scenario = params.get('dial-error') === 'unreachable' ? 'unreachable' : 'no-egress';
      job = { ...job, state: 'complete', path_error: null, previous_results: [], results: job.results.map(result => withDialFailure(result, scenario)) };
    }
    jobs.set(key, job);
  }
  return jobs.get(key) ?? null;
};

/** 链详情里的「重新检测」：排队 2.5 秒后开始，再过 7 秒完成，上一次的结果挪到 previous，与 Console 相同。 */
function startJob(app: string, chain: string, family: UnlockProbeSelection): NodeUnlockProbe {
  const last = jobOf(chain, family);
  const now = nowSecs();
  const shown = last?.results.length && !last.path_error ? last.results : (last?.previous_results ?? []);
  const queued: NodeUnlockProbe = {
    ...(last ?? { node_id: '', protocol: 'VLESS REALITY', app_id: app, chain_id: chain, family }),
    id: `mock-${chain}-${family}-${now}`,
    state: 'queued',
    trigger: 'manual',
    created_at: now,
    completed_at: null,
    next_run_at: now + 86_400,
    results: [],
    path_error: null,
    configuration_revision: 41,
    previous_results: shown,
    previous_completed_at: last?.completed_at ?? null,
  } as NodeUnlockProbe;
  const key = `${chain}/${family}`;
  jobs.set(key, queued);
  const refresh = () => void client.invalidateQueries({ queryKey: ['chain-unlock-probe', app, chain] });
  setTimeout(() => {
    if (jobs.get(key)?.id !== queued.id) return;
    jobs.set(key, { ...queued, state: 'running' });
    refresh();
  }, 2_500);
  setTimeout(() => {
    if (jobs.get(key)?.id !== queued.id) return;
    const done = nowSecs();
    jobs.set(key, { ...queued, state: 'complete', completed_at: done, next_run_at: done + 86_400, results: shown });
    refresh();
  }, 9_500);
  return queued;
}

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input instanceof Request ? input.url : input), location.origin);
  const path = url.pathname;
  const method = (init?.method ?? 'GET').toUpperCase();
  const now = nowSecs();
  const json = (body: unknown) => Response.json(body);

  const unlock = path.match(/^\/apps\/([^/]+)\/chains\/([^/]+)\/unlock-probe(\/capabilities)?$/);
  if (unlock) {
    const [, app, chain, capabilities] = unlock.map(part => (part ? decodeURIComponent(part) : part));
    await new Promise(resolve => setTimeout(resolve, method === 'POST' ? 280 : 40));
    if (capabilities) return json(capabilitiesOf(chain));
    const family: UnlockProbeSelection =
      url.searchParams.get('automatic') === 'true' ? 'auto' : url.searchParams.get('family') === 'v6' ? 'v6' : 'v4';
    return json(method === 'POST' ? startJob(app, chain, family) : jobOf(chain, family));
  }

  if (path === '/chains/unlock-probes') {
    if (params.has('error')) return Response.json({ error: '摘要暂不可用' }, { status: 503 });
    const all = chainSnapshot(false)
      .snapshot.apps.flatMap(app =>
        app.chains.map(chain => ({
          app_id: app.id,
          chain_id: chain.id,
          capabilities: capabilitiesOf(chain.id),
          probes: {
            v4: jobOf(chain.id, 'v4'),
            v6: jobOf(chain.id, 'v6'),
            auto: capabilitiesOf(chain.id).family_mode === 'automatic' ? jobOf(chain.id, 'auto') : null,
          },
        })),
      )
      .sort((a, b) => a.chain_id.localeCompare(b.chain_id));
    const after = url.searchParams.get('after');
    const remaining = all.filter(item => !after || item.chain_id > after);
    const limit = params.has('pages') ? 5 : 100;
    const chains = remaining.slice(0, limit);
    return json({ chains, next_cursor: remaining.length > limit ? chains.at(-1)!.chain_id : null });
  }
  if (path === '/branding') return json({ site_name: 'Brocade', icon_data_url: null });
  if (path === '/auth/state') return json({ initialized: true, public_open: role === 'public' });
  if (path === '/bootstrap') return json(session);
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
  if (path.startsWith('/compile/')) return json(COMPILE_VIEW);
  if (path === '/model/preview' && method === 'POST') {
    const { ops } = JSON.parse(String(init?.body ?? '{}')) as { ops?: ModelOp[] };
    return json({
      snapshot: withDetailFields(previewSnapshot(chainSnapshot(who.masked_assets), ops ?? [])),
      compile: COMPILE_VIEW,
      artifacts: { revision: 41, artifacts: [] },
    });
  }
  if (path === '/deployments/verify' && method === 'POST')
    return json({
      revision_id: 41,
      converged: true,
      summary: {
        total_targets: 24,
        changed_targets: 0,
        skipped_targets: 0,
        deferred_targets: 0,
        disruptive_targets: 0,
        max_wave: 0,
      },
      targets: [],
    });
  if (path === '/deployments') return json({ deployments: [] });
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
  if (path === '/model/snapshot') return json(withDetailFields(chainSnapshot(who.masked_assets)));
  if (path === '/settings') return json(settings);
  if (path === '/tenants') return json({ tenants: [{ id: TENANT, name: '平台' }] });
  if (path === '/certs') return json({ groups: [], nodes: [] });
  if (path === '/nodes/agent-state')
    return json({
      nodes: MACHINES.map(machine => {
        const state = agentState(machine, now);
        return { ...state, public_ipv4: masked(state.public_ipv4), public_ipv6: masked(state.public_ipv6) };
      }),
    });
  if (path === '/node-traffic') return json({ nodes: MACHINES.map(machine => trafficItem(machine, now)) });
  if (path === '/load/nodes/nic')
    return json({
      nodes: MACHINES.filter(reports).map(machine => nicView(machine, now, numberParam(url, 'windows') || 24)),
    });
  if (path === '/ping-probe/nodes/latest')
    return json({ interval_secs: 10, nodes: MACHINES.flatMap(machine => pingLatest(machine, now) ?? []) });
  if (path === '/links/health') return json({ hops: linkHealth(MACHINES, now) });
  if (path === '/links/mtu') return json({ links: [], nodes: [], default_mtu: 1420 });
  if (path === '/agent-log-policy') return json({ global: { max_mib: 64 }, nodes: [] });
  if (path === '/probes/e2e') return json(e2eProbes(now));
  if (path === '/users') return json({ users: [] });
  if (path === '/grant-probes/capability') return json({ available: false, reason: '示例不拨测' });
  return Response.json({ error: `示例未提供 ${path}` }, { status: 404 });
};

/** The realtime channels stay silent; every reading falls back to the polled queries. */
class SilentEventSource extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  readonly readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  constructor(readonly url: string) {
    super();
  }
  close() {}
}
Object.defineProperty(window, 'EventSource', { value: SilentEventSource, configurable: true });

if (!location.hash) history.replaceState(null, '', `${location.search}#/chains`);
if (params.get('view') !== 'cards') localStorage.setItem('brocade-console:chain-view', 'unlock');
else localStorage.setItem('brocade-console:chain-view', 'cards');
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <App />
  </QueryClientProvider>,
);
