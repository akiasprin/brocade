import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  DEFAULT_HOP_MUX,
  type DeploymentPlan,
  type DeploymentTargetDetail,
  type ExternalOutbound,
  type VpngateOverview,
} from '../../src/api';
import { ProvisionInstall } from '../../src/panes/nodes';
import { RuleEditor, type ForwardPeer } from '../../src/panes/rules';
import { DeployPane, RecordedArtifacts } from '../../src/panes/deploy';
import { VpngatePage } from '../../src/panes/vpngate';
import { SessionProvider } from '../../src/session';
import { theme } from '../../src/forge/theme';
import { draft } from '../../src/draft';
import { tunnelIdFromBytes } from '../../src/model-id';
import type { Win } from '../../src/wm/store';
import {
  appId,
  pairs,
  nodes as baseNodes,
  snapshot as baseSnapshot,
  settings,
  compiled,
  now,
} from './ui-regression-data';
import '../../src/styles.css';
const params = new URLSearchParams(location.search),
  scene = params.get('scene') ?? 'install';
if (theme.snapshot() !== (params.get('theme') === 'dark' ? 'dark' : 'light')) theme.toggle();
const longName =
  params.get('name') === 'ascii'
    ? 'Tokyo-Singapore-long-machine-name-'.repeat(4)
    : params.get('name') === 'short'
      ? '香港入口机器'
      : '东京-多地域备援-超长机器名称用于密集列表与换行检查-9';
const nodes = baseNodes.map((node, i) => ({ ...node, name: i === 0 ? longName : `部署机器 ${i}` }));
const mux = { ...DEFAULT_HOP_MUX, concurrency: 64, prewarm_workers: 1, reuse_threshold: 32 };
const outbound: ExternalOutbound = {
  id: tunnelIdFromBytes(new Uint8Array([1, 2, 3, 4])),
  tenant: 'platform',
  name: '审查隧道 2',
  address: '192.0.2.100',
  port: 443,
  protocol: { t: 'http_connect', v: { credential: '' } },
  security: { t: 'none' },
  bindings: [],
};
const rules =
  scene === 'proxy'
    ? [{ m: { t: 'any' as const }, a: { t: 'proxy' as const, outbound: outbound.id } }]
    : [
        {
          m: { t: 'any' as const },
          a: {
            t: 'forward' as const,
            to: nodes[1].node_id,
            dial: { t: 'overlay' as const },
            pool: { t: 'mux' as const, v: mux },
          },
        },
      ];
const source = { chain: pairs[0].chainId, node: nodes[0].node_id, accept: null, hop_in: null, rules };
const relay = {
  ...source,
  node: nodes[1].node_id,
  hop_in: { port: 22001, security: { t: 'none' as const } },
  rules: [{ m: { t: 'any' as const }, a: { t: 'egress' as const, send_through: null } }],
};
const snapshot = {
  ...baseSnapshot,
  snapshot: {
    ...baseSnapshot.snapshot,
    external_outbounds: [outbound],
    apps: [{ ...baseSnapshot.snapshot.apps[0], steps: [source, relay] }],
  },
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
draft.init('followup-layout');
const overview: VpngateOverview = {
  admission_policy: {
    minimum_successful_sources: 1,
    country_policy: 'any_match',
    risk_decision_policy: 'all_available_pass',
    provider_rules: [
      { provider: 'proxycheck', maximum_score: 80 },
      { provider: 'ffraud', maximum_score: 80 },
      { provider: 'iplogs', maximum_score: 80 },
    ],
  },
  status: {
    enabled: true,
    interval_secs: 900,
    probe_success_cooldown_secs: 1800,
    probe_performance_cooldown_secs: 21600,
    probe_shard_rotation_secs: 21600,
    source_url: 'https://www.vpngate.net/api/iphone/',
    next_sync_at_unix_secs: now / 1000 + 900,
    syncing: false,
    last_success_run_id: 18,
    last_error_code: null,
    last_error_detail: null,
    current_servers: 128,
    retained_servers: 128,
    retained_observations: 256,
  },
  sync_history: [
    {
      finished_at_unix_secs: now / 1000,
      current_servers: 128,
      first_seen_servers: 128,
      accepted_rows: 128,
      rejected_rows: 0,
    },
  ],
  countries: [
    {
      country_code: 'JP',
      country_name: 'Japan',
      current_servers: 128,
      retained_servers: 128,
      measured_successful: 0,
      candidate_servers: 0,
    },
  ],
};
const targets: DeploymentTargetDetail[] = Array.from({ length: 121 }, (_, i) => ({
  node_id: `recorded-${i}`,
  status: 'succeeded',
  error: null,
  wave: 0,
  disruptive: true,
  dispatched_at: null,
  verdict: null,
  desired_structure: {
    xray: { state: 'present', sha256: 'a'.repeat(64), content: '{"inbounds":[],"outbounds":[]}\n' },
  },
  observed_before: { xray: { state: 'present', sha256: 'b'.repeat(64), content: '{"inbounds":[]}\n' } },
  observed_after: null,
}));
const plan: DeploymentPlan = {
  revision: 7,
  base_revision_id: 6,
  warnings: [],
  summary: {
    total_targets: 121,
    changed_targets: 121,
    skipped_targets: 0,
    deferred_targets: 0,
    disruptive_targets: 121,
    max_wave: 27,
  },
  targets: Array.from({ length: 121 }, (_, i) => ({
    node_id: `recorded-${i}`,
    status: 'pending',
    wave: i % 28,
    disruptive: true,
    actions: ['apply-xray'],
  })),
};
for (const [key, value] of [
  [['nodes'], { nodes }],
  [['snapshot'], snapshot],
  [['settings'], settings],
  [['compile', 7], compiled],
  [
    ['revisions'],
    {
      current_revision: 7,
      revisions: [
        {
          id: 7,
          status: 'committed',
          has_snapshot: true,
          current: true,
          author: 'fixture',
          created_at: new Date(now).toISOString(),
        },
      ],
    },
  ],
  [['certs'], { domain: null, groups: [], nodes: [], sealing_available: true }],
  [['vpngate'], overview],
  [['vpngate', 'runtimes'], []],
  [['vpngate', 'country', 'JP'], { items: [], total: 0, page: 1, page_size: 100 }],
  [['deployment-settings'], { unattended: false, xray_handoff_beta: false }],
  [['deployments'], { deployments: [{ id: 1, revision_id: 6, status: 'succeeded' }] }],
  [['plan', 7], plan],
  [['plan-artifacts', 7, true], { revision: 7, targets: [] }],
  [['artifact-index', 7], { revision: 7, artifacts: [] }],
  [['artifact-index', 6], { revision: 6, artifacts: [] }],
] as [readonly unknown[], unknown][])
  client.setQueryData(key, value);
// Keep this acceptance fixture disconnected from business writes.
window.fetch = async () =>
  Response.json({ error: 'No network operation is provided by this fixture' }, { status: 404 });
Object.assign(window, { followupDraftOps: () => draft.ops() });
function Preview() {
  const [navigation, setNavigation] = useState('');
  const peers: ForwardPeer[] = [
    {
      id: nodes[1].node_id,
      name: nodes[1].name,
      public_ipv4: nodes[1].public_ipv4,
      public_ipv6: null,
      public_ipv4_nat: false,
      public_ipv6_nat: false,
      step: relay,
      where: 'next',
      blocked: null,
    },
  ];
  const win: Win = {
    id: 9991,
    key: 'tab:deploy',
    title: '发布预览',
    x: 0,
    y: 0,
    w: 1000,
    h: 800,
    z: 1,
    min: false,
    home: 'desk',
    data: { drill: { p: 'plan', revision: 7, key: 'fixture-plan' } },
  };
  return (
    <div className={`forge${innerWidth < 900 ? ' narrow' : ''}`}>
      <main className="fg-desk" style={{ padding: 12 }}>
        {navigation ? (
          <p role="status">{navigation}</p>
        ) : scene === 'install' ? (
          <ProvisionInstall node={nodes[0].node_id} go={drill => setNavigation(JSON.stringify(drill))} />
        ) : scene === 'mux' || scene === 'proxy' ? (
          <section className="panel config-panel rule-sheet-card node-chain-sheet">
            <RuleEditor
              appId={appId}
              chainId={pairs[0].chainId}
              nodeId={nodes[0].node_id}
              initial={rules}
              accept={null}
              peers={peers}
              isForwardTarget={false}
              fallback={{ rules: [], pending: false }}
            />
          </section>
        ) : scene === 'catalog' ? (
          <VpngatePage />
        ) : scene === 'artifacts' ? (
          <div className="cg-main">
            <RecordedArtifacts targets={targets} range="R6 → R7" />
          </div>
        ) : (
          <DeployPane win={win} />
        )}
      </main>
    </div>
  );
}
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: 'followup-layout',
          role: 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: false,
        },
        initial: { node_count: 12, chain_group_count: [] },
      }}
    >
      <Preview />
    </SessionProvider>
  </QueryClientProvider>,
);
