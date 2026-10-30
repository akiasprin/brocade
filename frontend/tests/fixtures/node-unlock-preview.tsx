import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ChainsPane } from '../../src/panes/chains';
import { NodesPane } from '../../src/panes/nodes';
import { SessionProvider } from '../../src/session';
import { theme } from '../../src/forge/theme';
import { nodes, snapshot, settings, compiled, report, appId, pairs } from './ui-regression-data';
import { UNLOCK_SERVICE_NAMES } from '../../src/panes/node-unlock';
import type { NodeUnlockProbe, UnlockDnsPath, UnlockConnectionObservation, UnlockPart } from '../../src/api';
import type { Win } from '../../src/wm/store';
import { SCENES, type SceneKey } from './unlock-mockup-data';
import { youtubeSplitResult } from './youtube-unlock-data';
import { withDialFailure, type DialScenario } from './unlock-dial-data';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const scene = params.get('scene') as SceneKey | null;
const node = {
  ...nodes[0],
  name: '香港 · 服务出口',
  public_ipv4_country: 'HK',
  agent_protocol_version: params.has('old') ? 45 : 50,
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
for (const [key, value] of [
  [['nodes'], { nodes: [node] }],
  [['snapshot'], snapshot],
  [['settings'], settings],
  [['revisions'], { current_revision: 7, revisions: [] }],
  [['compile', 7], compiled],
  [['e2e-probes'], { chains: [] }],
  [['deployments'], { deployments: [] }],
  [['certs'], { groups: [], nodes: [] }],
] as [unknown[], unknown][])
  client.setQueryData(key, value);
const results = Object.keys(UNLOCK_SERVICE_NAMES).map(service => {
  if (service === 'youtube_premium' && params.has('youtube-details')) {
    const result = structuredClone(youtubeSplitResult);
    if (params.has('youtube-partial'))
      result.youtube!.cdn = {
        http_status: null,
        node: null,
        airport: null,
        country: null,
        error: 'timeout',
        elapsed_ms: 0,
      };
    return result;
  }
  const state = ['chatgpt', 'claude', 'gemini'].includes(service)
    ? 'unsupported_region'
    : service === 'tiktok'
      ? 'failed'
      : service === 'reddit'
        ? 'restricted'
        : 'available';
  return {
    service,
    ...(params.has('client-dns') && service === 'youtube_premium' ? { client_dns: true } : {}),
    status: state,
    code:
      state === 'unsupported_region'
        ? 'region_unsupported'
        : state === 'failed'
          ? 'unrecognized_response'
          : state === 'restricted'
            ? 'access_denied'
            : 'web_available',
    region: ['tiktok', 'reddit'].includes(service)
      ? null
      : params.get('scope') === 'chain'
        ? service === 'chatgpt'
          ? 'TW'
          : service === 'netflix'
            ? 'JP'
            : 'HK'
        : 'HK',
  };
}) as NodeUnlockProbe['results'];
let job: NodeUnlockProbe | null = params.has('empty')
  ? null
  : {
      id: 'test-job',
      node_id: node.node_id,
      next_run_at: 1792987280,
      app_id: appId,
      chain_id: pairs[0].chainId,
      protocol: 'VLESS REALITY',
      state: params.has('running') ? 'running' : params.has('expired') ? 'expired' : 'complete',
      created_at: 1792900800,
      completed_at: params.has('running') ? null : 1792900880,
      results: params.has('running') || params.has('expired') ? [] : results,
    };
if (scene && SCENES[scene]) {
  job = SCENES[scene].job(params.get('scope') === 'chain' ? 'chain' : 'node');
  node.agent_protocol_version = SCENES[scene].protocol;
}
if (job) job.family = 'v4';
if (job && params.has('family-unavailable')) {
  job.path_error = 'family_unavailable';
  job.previous_results = [];
}
if (!params.has('old') && scene !== 'old') node.agent_protocol_version = 50;
if (job && params.has('previous-failed'))
  job.previous_results = job.results.map(result => ({
    service: result.service,
    status: 'failed',
    code: 'network_error',
    region: null,
  }));
// Each address family has independent backend state; the fixture exercises the real panel.
const v6 = job ? structuredClone(job) : null;
if (v6) {
  v6.family = 'v6';
  v6.id += '-v6';
  if (v6.completed_at) v6.completed_at += 7200;
  if (v6.next_run_at) v6.next_run_at += 7200;
  const project = (items: NodeUnlockProbe['results']) =>
    items.map(item => {
      const result = structuredClone(item);
      if (result.region) result.region = 'JP';
      if (result.youtube) {
        result.youtube.webpage.privacy_region = 'JP';
        result.youtube.premium = {
          ...result.youtube.premium,
          status: 'available',
          code: 'web_available',
          privacy_region: 'JP',
        };
        result.status = 'available';
        result.code = 'web_available';
        result.region = 'JP';
        result.youtube.cdn = {
          http_status: 200,
          node: 'nrt12s15',
          airport: 'NRT',
          country: 'JP',
          error: null,
          elapsed_ms: 230,
        };
      }
      if (params.has('no-ipv6')) {
        result.status = 'failed';
        result.code = 'network_error';
        result.region = null;
        if (result.youtube) {
          const failed = {
            status: 'failed' as const,
            code: 'network_error' as const,
            http_status: null,
            page_region: null,
            topbar_region: null,
            privacy_region: null,
          };
          result.youtube = {
            webpage: failed,
            premium: failed,
            cdn: { http_status: null, node: null, airport: null, country: null, error: 'network_error', elapsed_ms: 0 },
          };
        }
      }

      return result;
    });
  v6.results = project(v6.results);
  if (v6.previous_results) v6.previous_results = project(v6.previous_results);
}
// Measured DNS provenance fixtures exercise production A components.
if (params.has('dns-fallback') && v6) {
  for (const result of v6.results) {
    if (!['chatgpt', 'claude', 'gemini'].includes(result.service)) continue;
    result.dns_path = {
      exit: 'exit-hk',
      normal_family: 'v4',
      fallback: params.has('dns-stop') ? 'stop' : 'machine',
      rules: [
        {
          selector: { t: 'geosite', v: ['category-ai-!cn'] },
          resolution: {
            address: '198.51.100.53',
            port: 53,
            transport: 'tcp',
            address_strategy: params.has('dns-fixed') ? 'use_ipv4' : 'use_ip',
            fallback: params.has('dns-stop') ? 'stop' : 'machine',
          },
        },
      ],
    };
    result.dns_parts = [{ part: 'service', kind: 'fallback', paths: [result.dns_path], exits: ['exit-hk'] }];
    if (params.has('dns-stop')) {
      result.status = 'skipped';
      result.code = 'no_ipv6_dns';
      result.region = null;
    }
  }
}
if (params.has('connection-evidence') && job) {
  node.agent_protocol_version = 57;
  const youtube = job.results.find(result => result.service === 'youtube_premium');
  if (youtube) {
    const path: UnlockDnsPath = {
      exit: 'akile-mol1',
      normal_family: 'v6',
      fallback: 'machine',
      answer_from: 'machine',
      reason: 'no_address',
      rules: [
        {
          selector: { t: 'geosite', v: ['google'] },
          resolution: {
            address: '1.1.1.1',
            port: 53,
            transport: 'udp',
            address_strategy: 'use_ipv6',
            fallback: 'machine',
          },
        },
      ],
    };
    const mixed = params.has('connection-mixed');
    youtube.youtube = structuredClone(youtubeSplitResult.youtube);
    youtube.status = 'available';
    youtube.code = 'web_available';
    youtube.region = 'TR';
    for (const part of ['webpage', 'premium'] as const)
      youtube.youtube![part] = {
        status: 'available',
        code: 'web_available',
        http_status: 200,
        page_region: 'TR',
        topbar_region: 'TR',
        privacy_region: 'TR',
      };
    youtube.dns_parts = [
      { part: 'webpage', kind: 'fallback', paths: [path], exits: ['akile-mol1'] },
      {
        part: 'premium',
        kind: mixed ? 'partial' : 'fallback',
        paths: [path],
        exits: mixed ? ['akile-mol1', 'exit-taiwan'] : ['akile-mol1'],
      },
      { part: 'cdn', kind: 'direct', paths: [], exits: ['exit-taiwan'] },
    ];
    const connection = (
      part: UnlockPart,
      id: number,
      exit: string,
      domain: string,
      fallback: boolean,
    ): UnlockConnectionObservation => ({
      part,
      version: 2,
      id: id.toString(16).padStart(32, '0'),
      domain,
      exit,
      outbound: 'out:forward:probe-v4',
      profile: 'a'.repeat(64),
      normal_strategy: 'UseIP',
      family: 'v4',
      connected: true,
      complete: true,
      lookups: [
        {
          version: 1,
          domain,
          strategy: 'FORCE_IP4',
          complete: true,
          steps: fallback
            ? [
                { tag: 'dns:egress:23aad4af493f', scoped: true, outcome: 'empty', other: true },
                { tag: 'xray.system.synthetic', scoped: false, outcome: 'answer', other: false },
              ]
            : [{ tag: 'xray.system.synthetic', scoped: false, outcome: 'answer', other: false }],
        },
      ],
    });
    youtube.connection_observations = [
      connection('webpage', 1, 'akile-mol1', 'www.youtube.com', true),
      connection('premium', 2, 'akile-mol1', 'www.youtube.com', true),
      connection('cdn', 3, 'exit-taiwan', 'redirector.googlevideo.com', false),
      ...(mixed ? [connection('premium', 4, 'exit-taiwan', 'www.youtube.com', false)] : []),
    ];
    youtube.connection_requests = { webpage: 1, premium: mixed ? 2 : 1, cdn: 1 };
  }
}
if (params.has('dns-unclassified') && v6) {
  node.agent_protocol_version = 57;
  for (const reading of v6.results) {
    if (!['chatgpt', 'claude', 'gemini', 'youtube_premium'].includes(reading.service)) continue;
    reading.status = 'skipped';
    reading.code = 'no_ipv6_dns';
    reading.region = null;
    const parts: UnlockPart[] = reading.service === 'youtube_premium' ? ['webpage', 'premium', 'cdn'] : ['service'];
    reading.dns_parts = parts.map(part => ({ part, kind: 'unknown', paths: [], exits: [] }));
    if (reading.service === 'youtube_premium') {
      const page = {
        status: 'skipped' as const,
        code: 'no_ipv6_dns' as const,
        http_status: null,
        page_region: null,
        topbar_region: null,
        privacy_region: null,
      };
      reading.youtube = {
        webpage: page,
        premium: { ...page },
        cdn: { node: null, airport: null, country: null, error: 'no_ipv6_dns', http_status: null, elapsed_ms: 10 },
      };
    }
  }
}
if (params.has('youtube-region') && job) {
  const region = params.get('youtube-region') === 'CN' ? 'CN' : 'MO';
  node.agent_protocol_version = 57;
  const reading = job.results.find(result => result.service === 'youtube_premium');
  if (reading) {
    Object.assign(reading, structuredClone(youtubeSplitResult), { region });
    for (const part of ['webpage', 'premium'] as const) {
      reading.youtube![part].status = 'unsupported_region';
      reading.youtube![part].code = 'region_unsupported';
      reading.youtube![part].privacy_region = region;
    }
  }
}
const dialScenario = params.get('dial-error');
if (v6 && ['no-egress', 'unreachable', 'mixed'].includes(dialScenario ?? '')) {
  node.agent_protocol_version = 57;
  v6.results = v6.results.map(result => withDialFailure(result, dialScenario as DialScenario));
}
const jobs = { v4: job, v6 };
window.fetch = async (input, init) => {
  const url = new URL(String(input), location.origin);
  const path = url.pathname;
  if (path.includes('/unlock-probe')) {
    const family = url.searchParams.get('family') === 'v6' ? 'v6' : 'v4';
    if (init?.method === 'POST') {
      const before = jobs[family];
      jobs[family] = {
        ...before,
        family,
        id: 'new-job-' + family,
        node_id: node.node_id,
        state: 'queued',
        created_at: Math.floor(Date.now() / 1000),
        completed_at: null,
        results: [],
        previous_results: before?.results ?? [],
        previous_completed_at: before?.completed_at ?? null,
      };
    }
    return Response.json(jobs[family]);
  }
  if (path === '/nodes/agent-state') return Response.json({ nodes: [node] });
  if (path.startsWith('/load/nodes/')) return Response.json({ ...report, node_id: node.node_id });
  if (path.startsWith('/usage/node-series')) return Response.json({ nodes: [], since: '', month_start: '' });
  if (path.startsWith('/ping-probe/')) return Response.json({ node_id: node.node_id, targets: [] });
  if (path.startsWith('/probes/e2e')) return Response.json({ chains: [] });
  if (path === '/certs') return Response.json({ groups: [], nodes: [] });
  if (path.startsWith('/deployments')) return Response.json({ deployments: [] });
  if (path === '/model/snapshot') return Response.json(snapshot);
  if (path === '/settings') return Response.json(settings);
  return Response.json({ error: 'not part of the unlock fixture' }, { status: 404 });
};
const win: Win = {
  id: 9100,
  key: 'tab:nodes',
  title: '机器',
  x: 0,
  y: 0,
  w: 1200,
  h: 900,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'node', id: node.node_id, tab: 'unlock' } },
};
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: 'preview',
          role: params.has('readonly') ? 'readonly' : 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: false,
        },
        initial: { node_count: 1, chain_group_count: [] },
      }}
    >
      <div className={`forge${innerWidth < 900 ? ' narrow' : ''}`}>
        <main className="fg-desk" style={{ padding: 16 }}>
          {params.get('scope') === 'chain' ? (
            <ChainsPane win={{ ...win, data: { drill: { p: 'chain', app: appId, chain: pairs[0].chainId } } }} />
          ) : (
            <NodesPane win={win} bare />
          )}
        </main>
      </div>
    </SessionProvider>
  </QueryClientProvider>,
);
