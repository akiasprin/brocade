import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import {
  AGENT_PROTOCOL_VERSION,
  type ConsoleSnapshot,
  type ExternalOutbound,
  type Rule,
  type VpngateServerView,
} from '../src/api';
import { draft } from '../src/draft';
import { RuleEditor } from '../src/panes/rules';
import { VpngateRuleMenu } from '../src/panes/vpngate-rule-menu';

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

const countries = [
  {
    country_code: 'JP',
    country_name: 'Japan',
    current_servers: 10,
    retained_servers: 12,
    measured_successful: 2,
    candidate_servers: 2,
  },
  {
    country_code: 'HK',
    country_name: 'Hong Kong',
    current_servers: 1,
    retained_servers: 1,
    measured_successful: 0,
    candidate_servers: 0,
  },
  {
    country_code: 'ZZ',
    country_name: 'Reserved',
    current_servers: 1,
    retained_servers: 1,
    measured_successful: 1,
    candidate_servers: 1,
  },
];
const servers: VpngateServerView[] = Array.from({ length: 18 }, (_, index) => ({
  id: `vpn-jp-${index}`,
  hostname: `relay-${index}`,
  ip: `192.0.2.${index + 1}`,
  country_code: 'JP',
  country_name: 'Japan',
  ping_ms: null,
  catalog_speed_bps: 0,
  vpn_sessions: 0,
  last_seen_at_unix_secs: 0,
  seen_in_latest_sync: index < 10,
  active: index < 2,
  measured_nodes: 0,
  successful_samples: 0,
  latest_probe_status: null,
  latest_exit_ip: null,
  latest_exit_country_code: null,
  latest_connect_ms: null,
  latest_download_bps: null,
  latest_ip_scores: [],
  latest_ip_networks: [],
  latest_error_code: null,
  latest_probed_at_unix_secs: null,
  latest_successful_probed_at_unix_secs: null,
  intelligence_verified_at_unix_secs: null,
  intelligence_stale: false,
}));

const directoryRequest = {
  page: 1,
  page_size: 100,
  search: '',
  filter: 'all',
  sort: 'candidate',
} as const;

function clientWithCatalog() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['vpngate'], {
    countries,
    status: {},
    manual_pools_supported: true,
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
  });
  client.setQueryData(['vpngate', 'country', 'JP', directoryRequest], {
    items: servers,
    total: servers.length,
    page: 1,
    page_size: 100,
  });
  return client;
}

function mountEditor(outbounds: ExternalOutbound[] = []) {
  const client = clientWithCatalog();
  const snapshot: ConsoleSnapshot = {
    snapshot: {
      revision: 1,
      external_outbounds: outbounds,
      apps: [
        {
          id: 'app',
          label: '线路',
          chains: [{ id: 'chain', name: '链', tenant: 'platform' }],
          steps: [],
          ingresses: [],
          fronts: [],
          grants: [],
        },
      ],
    },
    node_egress_dns: [],
    redacted: false,
  };
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
  client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
  client.setQueryData(['nodes'], {
    nodes: [
      {
        node_id: 'edge',
        name: '接入',
        tenant_id: 'platform',
        public_ipv4: '192.0.2.1',
        egress_allowed: true,
        lifecycle_phase: 'active',
        operationally_isolated: false,
        agent_protocol_version: AGENT_PROTOCOL_VERSION,
        runtime_report_fresh: true,
        runtime_versions: { openvpn: 'OpenVPN 2.6' },
      },
    ],
  });
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url === '/model/preview') {
        const pending = draft
          .ops()
          .flatMap(op =>
            op.op === 'upsert_external_outbound'
              ? [{ ...op.outbound, tenant: op.outbound.tenant_id, bindings: [] }]
              : [],
          );
        return new Response(
          JSON.stringify({
            snapshot: {
              ...snapshot,
              snapshot: { ...snapshot.snapshot, external_outbounds: [...outbounds, ...pending] },
            },
          }),
        );
      }
      if (url.startsWith('/revisions')) return new Response(JSON.stringify({ current_revision: null, revisions: [] }));
      throw new Error(`Unexpected request: ${url}`);
    }),
  );
  const initial: Rule[] = [
    { m: { t: 'any' }, a: { t: 'forward', to: '', dial: { t: 'overlay' }, pool: { t: 'none' } } },
  ];
  render(
    <QueryClientProvider client={client}>
      <RuleEditor
        appId="app"
        chainId="chain"
        nodeId="edge"
        initial={initial}
        accept={null}
        peers={[]}
        isForwardTarget={false}
        fallback={{ rules: [], pending: false }}
      />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: /选择内部节点或代理出站/ }));
  fireEvent.click(screen.getByRole('button', { name: 'VPN Gate' }));
}

it('selects a Chinese region directly without preregistration; saves pool before its rule', async () => {
  mountEditor();
  expect(screen.getByRole('button', { name: '日本 自动节点池' })).toBeTruthy();
  expect(screen.getByRole('button', { name: '香港 自动节点池' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /ZZ|未知地区/ })).toBeNull();
  fireEvent.change(screen.getByRole('textbox', { name: '搜索 VPN Gate 地区' }), { target: { value: '香港' } });
  expect(screen.queryByRole('button', { name: '日本 自动节点池' })).toBeNull();
  expect(draft.ops()).toEqual([]);
  fireEvent.click(screen.getByRole('button', { name: '香港 自动节点池' }));
  expect(draft.ops()).toEqual([]);
  fireEvent.click(screen.getByRole('button', { name: '保存到草稿' }));
  await waitFor(() =>
    expect(draft.ops().map(op => op.op)).toEqual(['upsert_external_outbound', 'put_step', 'prune_chain']),
  );
  const operation = draft.ops()[0];
  if (operation.op !== 'upsert_external_outbound') throw Error('expected provider selection');
  expect(operation.outbound.protocol).toMatchObject({ t: 'vpngate', v: { country_code: 'HK', max_candidates: 16 } });
  expect(operation.outbound.name).toContain('香港');
  expect(draft.ops()[1]).toMatchObject({ step: { rules: [{ a: { t: 'proxy', outbound: operation.outbound.id } }] } });
});

it('selects multiple nodes including those outside the automatic shortlist and stages only the final pool', async () => {
  mountEditor();
  fireEvent.click(screen.getByRole('button', { name: '日本 自动节点池' }));
  fireEvent.click(screen.getByRole('button', { name: /自动池/ }));
  fireEvent.click(screen.getByRole('button', { name: 'VPN Gate' }));
  fireEvent.click(screen.getByRole('button', { name: '展开 日本 节点列表' }));
  expect(screen.getAllByRole('checkbox')).toHaveLength(18);
  expect((screen.getByRole('button', { name: '使用手动节点池' }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('checkbox', { name: '选择 relay-11 (192.0.2.12)' }));
  fireEvent.click(screen.getByRole('checkbox', { name: '选择 relay-0 (192.0.2.1)' }));
  expect(draft.ops()).toEqual([]);
  fireEvent.click(screen.getByRole('button', { name: '使用手动节点池' }));
  fireEvent.click(screen.getByRole('button', { name: '保存到草稿' }));
  await waitFor(() => expect(draft.ops()).toHaveLength(3));
  expect(draft.ops()[0]).toMatchObject({
    outbound: {
      protocol: { t: 'vpngate', v: { country_code: 'JP', server_ids: ['vpn-jp-0', 'vpn-jp-11'], max_candidates: 2 } },
    },
  });
});

it('reuses an existing country pool without changing its thresholds or creating another resource', async () => {
  const pool: ExternalOutbound = {
    id: 'vpngate-1111-1111',
    tenant: 'platform',
    name: '原有日本池',
    address: 'managed.vpngate.invalid',
    port: 1,
    protocol: {
      t: 'vpngate',
      v: { country_code: 'JP', max_candidates: 3, max_connect_ms: 5000, min_download_bps: 2_000_000 },
    },
    security: { t: 'none' },
    bindings: [],
  };
  mountEditor([pool]);
  fireEvent.click(screen.getByRole('button', { name: '日本 自动节点池' }));
  fireEvent.click(screen.getByRole('button', { name: '保存到草稿' }));
  await waitFor(() => expect(draft.ops().map(op => op.op)).toEqual(['put_step', 'prune_chain']));
  expect(draft.ops()[0]).toMatchObject({ op: 'put_step', step: { rules: [{ a: { t: 'proxy', outbound: pool.id } }] } });
});

it('caps the manual pool at sixteen and discards checkbox changes on back navigation', () => {
  const selected = vi.fn();
  render(
    <QueryClientProvider client={clientWithCatalog()}>
      <VpngateRuleMenu selected={null} onBack={vi.fn()} onSelect={selected} />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: '展开 日本 节点列表' }));
  screen
    .getAllByRole('checkbox')
    .slice(0, 16)
    .forEach(checkbox => fireEvent.click(checkbox));
  expect((screen.getAllByRole('checkbox')[16] as HTMLInputElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('button', { name: '返回 VPN Gate 地区列表' }));
  expect(selected).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: '展开 日本 节点列表' }));
  expect(screen.getAllByRole('checkbox').every(checkbox => !(checkbox as HTMLInputElement).checked)).toBe(true);
});

it('does not send a manual pool to an older Console that would silently ignore its members', () => {
  const client = clientWithCatalog();
  client.setQueryData(['vpngate'], { countries, status: {} });
  const selected = vi.fn();
  render(
    <QueryClientProvider client={client}>
      <VpngateRuleMenu selected={null} onBack={vi.fn()} onSelect={selected} />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: '展开 日本 节点列表' }));
  fireEvent.click(screen.getAllByRole('checkbox')[0]);
  fireEvent.click(screen.getByRole('button', { name: '使用手动节点池' }));
  expect(screen.getByRole('status').textContent).toContain('请先升级 Console');
  expect(selected).not.toHaveBeenCalled();
});

it('reports catalogue failures and does not replace the current rule selection', async () => {
  const client = clientWithCatalog();
  const selected = vi.fn();
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response('catalogue unavailable', { status: 503 })),
  );
  render(
    <QueryClientProvider client={client}>
      <VpngateRuleMenu selected={null} onBack={vi.fn()} onSelect={selected} />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: '展开 香港 节点列表' }));
  expect(screen.getByRole('status')).toBeTruthy();
  await screen.findByRole('button', { name: '重试节点列表' });
  expect((screen.getByRole('button', { name: '使用手动节点池' }) as HTMLButtonElement).disabled).toBe(true);
  expect(selected).not.toHaveBeenCalled();
});
