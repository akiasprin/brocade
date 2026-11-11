// Mount the production app on synthetic API data. No alternate navigation or styles.
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from '../../src/app';
import { theme } from '../../src/forge/theme';
import { installBackend } from './console-backend';
import { settings } from './unlock-data';
import type {
  AgentLogPolicyView,
  CertsView,
  DataRetentionView,
  DeploymentSettings,
  DistributionView,
  HostNetworkTuning,
  VpngateOverview,
} from '../../src/api';
import '../../src/styles.css';

// Complete settings responses keep navigation checks on the real page instead of its error state.
const settingsResponses = {
  '/site-usage/settings': { enabled: true },
  '/other-domain/settings': {
    enabled: true,
    top_n: 128,
    node_capacity: 4096,
    report_interval_secs: 30,
    max_domains_per_user: 1024,
  },
  '/sniffing-failed/settings': {
    enabled: true,
    top_n: 128,
    node_capacity: 4096,
    report_interval_secs: 30,
    max_targets_per_user: 1024,
  },
  '/certs': {
    sealing_available: true,
    domain: null,
    groups: [],
    nodes: [],
    scan: null,
    letsencrypt: 'https://acme.example.test/directory',
    letsencrypt_staging: 'https://staging.acme.example.test/directory',
  } satisfies CertsView,
  '/distribution': {
    stored: { agent_public_url: 'https://console.example.test', xray_version: null },
    effective: { agent_public_url: 'https://console.example.test', xray_version: '26.7.28' },
    xray_source_revision: 'fixture-source',
  } satisfies DistributionView,
  '/deployment-settings': {
    unattended: true,
    xray_handoff_beta: false,
    unlock_detection_mode: 'scheduled',
    unlock_detection_interval_hours: 24,
  } satisfies DeploymentSettings,
  '/agent-log-policy': {
    global: { agent_journal_mib: 100, xray_mib: 100, phantun_mib: 100 },
    nodes: [],
  } satisfies AgentLogPolicyView,
  '/data-retention': { policies: [] } satisfies DataRetentionView,
  '/host-network-tuning': {
    gro_flush_timeout_ns: 20000,
    napi_defer_hard_irqs: 2,
  } satisfies HostNetworkTuning,
  '/vpngate': {
    manual_pools_supported: true,
    status: {
      enabled: true,
      interval_secs: 900,
      source_url: 'https://catalog.example.test/',
      next_sync_at_unix_secs: 1_800_000_000,
      syncing: false,
      last_success_run_id: 18,
      last_error_code: null,
      last_error_detail: null,
      current_servers: 0,
      retained_servers: 0,
      retained_observations: 0,
    },
    countries: [],
    admission_policy: {
      minimum_successful_sources: 1,
      country_policy: 'any_match',
      risk_decision_policy: 'all_available_pass',
      provider_rules: (['proxycheck', 'ffraud', 'iplogs'] as const).map(provider => ({ provider, maximum_score: 80 })),
    },
  } satisfies VpngateOverview,
};

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
installBackend();
const backend = globalThis.fetch;
globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = new URL(input instanceof Request ? input.url : String(input), location.origin);
  const state = params.get('state') ?? 'idle';
  if (url.pathname === '/deployments/verify') console.info('[fixture] deployment-verification-read');
  if (url.pathname === '/settings') return Response.json(settings);
  if (url.pathname in settingsResponses)
    return Response.json(settingsResponses[url.pathname as keyof typeof settingsResponses]);
  if (url.pathname === '/tunnel-probes/capability')
    return Response.json({ available: false, reason: '本地夹具不拨测' });
  if (url.pathname.endsWith('/tunnels/custom-1234-5678/probe'))
    return Response.json({ error: '隧道尚无探测记录' }, { status: 404 });
  if (url.pathname === '/deployments' && url.searchParams.has('active_only'))
    return Response.json({
      deployments: state === 'deploying' ? [{ id: 42, active: true, awaiting_confirmation: false }] : [],
    });
  if (url.pathname === '/artifacts/index') {
    const revision = Number(url.searchParams.get('revision'));
    console.info(`[fixture] artifact-index-read ${revision}`);
    const current = revision === 41;
    return Response.json({
      revision,
      artifacts: [
        {
          target_kind: 'node',
          target_id: 'jp-iij',
          artifact_kind: 'xray',
          state: 'present',
          sha256: (state === 'pending' && current ? 'b' : 'a').repeat(64),
          byte_len: 1200,
        },
        {
          target_kind: 'node',
          target_id: 'jp-iij',
          artifact_kind: 'grants',
          state: 'present',
          sha256: (current ? 'c' : 'd').repeat(64),
          byte_len: 200,
        },
        ...(current
          ? [
              {
                target_kind: 'user',
                target_id: 'platform/new-user',
                artifact_kind: 'uri',
                state: 'present',
                sha256: 'e'.repeat(64),
                byte_len: 100,
              },
            ]
          : []),
      ],
    });
  }
  const response = await backend(input, init);
  if (url.pathname === '/revisions') {
    const value = await response.json();
    value.last_config_revision_id = ['pending', 'grants-published'].includes(state)
      ? value.current_revision - 1
      : value.current_revision;
    return Response.json(value);
  }
  if (url.pathname === '/model/snapshot') {
    const value = await response.json();
    // Canonical tunnel IDs exercise real detail routes rather than legacy fixture IDs.
    value.snapshot.external_outbounds[1].id = 'custom-1234-5678';
    value.snapshot.external_outbounds[1].protocol = { t: 'socks5', v: { username: '', credential: '' } };
    value.snapshot.external_outbounds[1].security = { t: 'none' };
    return Response.json(value);
  }
  if (url.pathname === '/deployments/verify' && state === 'pending') {
    const value = await response.json();
    value.summary.changed_targets = 3;
    return Response.json(value);
  }
  if (url.pathname === '/grants/automation' && state === 'grant') {
    const value = await response.json();
    value.failed_jobs = 2;
    value.last_error = '合成权限任务失败';
    return Response.json(value);
  }
  if (url.pathname === '/bootstrap') {
    const value = await response.json();
    value.who.operator_id = params.get('account') ?? 'admin';
    if (params.get('role') === 'readonly') value.who.role = 'readonly';
    if (params.get('role') === 'public') {
      value.who.role = 'readonly';
      value.who.operator_id = 'public';
    }
    return Response.json(value);
  }
  return response;
};

createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <App />
  </QueryClientProvider>,
);
