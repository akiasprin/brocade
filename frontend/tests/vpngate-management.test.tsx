import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  AGENT_PROTOCOL_VERSION,
  type ConsoleSnapshot,
  type NodeAgentStateItem,
  type VpngateOverview,
  type VpngateRuntimeView,
  type VpngateServerView,
} from '../src/api';
import { draft } from '../src/draft';
import { VpngatePage } from '../src/panes/vpngate';
import { SessionProvider } from '../src/session';
import type { VpngateRealtimeReport } from '../src/vpngate-realtime';

class RuntimeEvents {
  static current: RuntimeEvents;
  handlers = new Map<string, (event: { data: string }) => void>();
  onerror: (() => void) | null = null;
  constructor() {
    RuntimeEvents.current = this;
  }
  addEventListener(name: string, callback: (event: { data: string }) => void) {
    this.handlers.set(name, callback);
  }
  close() {}
  sample(activeId = 'vpn-jp-2', age = 0) {
    const now = Date.now();
    const report: VpngateRealtimeReport = {
      boot_id: 'boot',
      sequence: 9,
      sampled_at_unix_millis: now - age,
      pools: [
        {
          outbound_id: 'vpngate-aaaa-aaaa',
          country_code: 'JP',
          state: 'healthy',
          active_slot: 1,
          ready_standbys: 1,
          candidate_count: 10,
          consecutive_failures: 0,
          probes: 100,
          probe_failures: 4,
          failovers: 8,
          refill_attempts: 9,
          refill_failures: 1,
          refill_backoff_remaining_millis: 0,
        },
      ],
      backends: [
        {
          outbound_id: 'vpngate-aaaa-aaaa',
          slot: 1,
          role: 'active',
          state: 'healthy',
          server_id: activeId,
          consecutive_failures: 0,
          backoff_remaining_millis: 0,
        },
        {
          outbound_id: 'vpngate-aaaa-aaaa',
          slot: 0,
          role: 'standby',
          state: 'healthy',
          server_id: activeId === 'vpn-jp-2' ? 'vpn-jp-1' : 'vpn-jp-2',
          consecutive_failures: 0,
          backoff_remaining_millis: 0,
        },
      ],
      events: [],
    };
    this.handlers.get('sample')?.({
      data: JSON.stringify({
        node_id: 'hk-edge',
        received_at_unix_millis: now,
        sample: { sampled_at_unix_millis: now, vpngate: report },
      }),
    });
  }
}

const chartMocks = vi.hoisted(() => ({
  setOption: vi.fn(),
  resize: vi.fn(),
  dispose: vi.fn(),
  init: vi.fn(),
}));
chartMocks.init.mockReturnValue({
  setOption: chartMocks.setOption,
  resize: chartMocks.resize,
  dispose: chartMocks.dispose,
});
vi.mock('echarts/core', () => ({ use: vi.fn(), init: chartMocks.init }));
vi.mock('echarts/charts', () => ({ LineChart: {} }));
vi.mock('echarts/components', () => ({ GridComponent: {}, TooltipComponent: {} }));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

const initial = { node_count: 0, chain_group_count: [] };

const snapshot: ConsoleSnapshot = {
  snapshot: {
    revision: 7,
    apps: [
      {
        id: 'streaming',
        label: '流媒体',
        chains: [{ id: 'chain-jp', name: '日本线路', tenant: 'platform' }],
        steps: [
          {
            chain: 'chain-jp',
            node: 'hk-edge',
            accept: null,
            hop_in: null,
            rules: [
              {
                m: { t: 'geosite', v: ['netflix'] },
                a: { t: 'proxy', outbound: 'vpngate-aaaa-aaaa' },
              },
              {
                m: { t: 'geosite', v: ['disney'] },
                a: { t: 'proxy', outbound: 'vpngate-aaaa-aaaa' },
              },
            ],
          },
        ],
        ingresses: [],
        fronts: [],
        grants: [],
      },
    ],
    external_outbounds: [
      {
        id: 'vpngate-aaaa-aaaa',
        tenant: 'platform',
        name: '日本规则池',
        address: 'managed.vpngate.invalid',
        port: 1,
        protocol: {
          t: 'vpngate',
          v: {
            country_code: 'JP',
            max_connect_ms: 15_000,
            min_download_bps: 1_000_000,
            max_candidates: 10,
          },
        },
        security: { t: 'none' },
        bindings: [],
      },
      {
        id: 'vpngate-bbbb-bbbb',
        tenant: 'platform',
        name: '遗留未引用池',
        address: 'managed.vpngate.invalid',
        port: 1,
        protocol: {
          t: 'vpngate',
          v: {
            country_code: 'JP',
            max_connect_ms: 15_000,
            min_download_bps: 1_000_000,
            max_candidates: 10,
          },
        },
        security: { t: 'none' },
        bindings: [],
      },
    ],
  },
  node_egress_dns: [],
  redacted: false,
};

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
  intelligence_policy: {
    refresh_mode: 'on_change',
    refresh_interval_hours: 168,
    active_window_hours: 72,
    stale_policy: 'retain',
    stale_after_hours: 168,
  },
  status: {
    enabled: true,
    interval_secs: 900,
    probe_success_cooldown_secs: 1800,
    probe_performance_cooldown_secs: 21600,
    probe_shard_rotation_secs: 21600,
    source_url: 'https://www.vpngate.net/api/iphone/',
    next_sync_at_unix_secs: 1_800_000_000,
    syncing: false,
    last_success_run_id: 18,
    last_error_code: null,
    last_error_detail: null,
    current_servers: 27,
    retained_servers: 63,
    retained_observations: 412,
  },
  sync_history: [
    {
      finished_at_unix_secs: 1_799_998_200,
      current_servers: 21,
      first_seen_servers: 21,
      accepted_rows: 21,
      rejected_rows: 2,
    },
    {
      finished_at_unix_secs: 1_799_999_100,
      current_servers: 24,
      first_seen_servers: 4,
      accepted_rows: 24,
      rejected_rows: 1,
    },
    {
      finished_at_unix_secs: 1_800_000_000,
      current_servers: 27,
      first_seen_servers: 3,
      accepted_rows: 27,
      rejected_rows: 0,
    },
  ],
  countries: [
    {
      country_code: 'JP',
      country_name: 'Japan',
      current_servers: 18,
      retained_servers: 42,
      measured_successful: 3,
      candidate_servers: 2,
    },
    {
      country_code: 'KR',
      country_name: 'Korea Republic of',
      current_servers: 9,
      retained_servers: 21,
      measured_successful: 0,
      candidate_servers: 0,
    },
  ],
};

const servers: VpngateServerView[] = Array.from({ length: 12 }, (_, index) => {
  const ordinal = index + 1;
  return {
    id: `vpn-jp-${ordinal}`,
    hostname: `public-vpn-${ordinal}`,
    ip: `192.0.2.${9 + ordinal}`,
    country_code: 'JP',
    country_name: 'Japan',
    ping_ms: 21,
    catalog_speed_bps: 300_000_000,
    vpn_sessions: 4,
    last_seen_at_unix_secs: 1_799_999_900,
    seen_in_latest_sync: ordinal <= 16,
    active: ordinal <= 2,
    candidate_rank: ordinal,
    pareto_layer: ordinal <= 2 ? 1 : 2,
    global_download_bps: 38_000_000,
    global_connect_ms: 640,
    measured_nodes: 1,
    successful_samples: 2,
    latest_probe_status: 'succeeded',
    latest_exit_ip: '198.51.100.20',
    latest_exit_country_code: 'JP',
    latest_connect_ms: 640,
    latest_download_bps: 38_000_000,
    latest_ip_scores: [
      { provider: 'proxycheck', score: 8, country_code: 'JP' },
      { provider: 'ffraud', score: 6, country_code: 'JP' },
      { provider: 'iplogs', score: 7, country_code: 'JP' },
    ],
    latest_ip_networks: [
      { provider: 'proxycheck', isp: 'Example ISP', network_type: 'business' },
      { provider: 'ffraud', isp: 'Example ISP', network_type: 'business' },
      { provider: 'iplogs', isp: 'Example ISP', network_type: 'business' },
    ],
    latest_error_code: null,
    latest_probed_at_unix_secs: 1_799_999_940,
    latest_successful_probed_at_unix_secs: 1_799_999_940,
    intelligence_verified_at_unix_secs: 1_799_999_945,
    intelligence_stale: false,
  };
});

const runtimes: VpngateRuntimeView[] = [
  {
    node_id: 'hk-edge',
    node_name: '香港接入',
    tenant_id: 'platform',
    outbound_id: 'vpngate-aaaa-aaaa',
    outbound_name: 'VPN Gate · Japan',
    country_code: 'JP',
    automatic_pool: true,
    runtime_status: 'running',
    selected_server_id: 'vpn-jp-1',
    selected_hostname: 'public-vpn-1',
    reported_at_unix_secs: 1_799_999_950,
    latest_probe_status: 'succeeded',
    latest_exit_ip: '198.51.100.20',
    latest_exit_country_code: 'JP',
    latest_connect_ms: 820,
    latest_download_bps: 42_000_000,
    latest_ip_scores: [
      { provider: 'proxycheck', score: 8, country_code: 'JP' },
      { provider: 'ffraud', score: 6, country_code: 'JP' },
      { provider: 'iplogs', score: 7, country_code: 'JP' },
    ],
    latest_ip_networks: [
      { provider: 'proxycheck', isp: 'Example ISP', network_type: 'business' },
      { provider: 'ffraud', isp: 'Example ISP', network_type: 'business' },
      { provider: 'iplogs', isp: 'Example ISP', network_type: 'business' },
    ],
    latest_error_code: null,
    latest_error_detail: null,
    latest_probed_at_unix_secs: 1_799_999_940,
    latest_successful_probed_at_unix_secs: 1_799_999_940,
    intelligence_verified_at_unix_secs: 1_799_999_945,
    intelligence_stale: false,
    switch_request_id: null,
    switch_status: null,
    switch_previous_server_id: null,
    switch_previous_hostname: null,
    switch_selected_server_id: null,
    switch_cooldown_until_unix_secs: null,
    switch_error_detail: null,
  },
];

const nodes = [
  {
    node_id: 'hk-edge',
    name: '香港接入',
    lifecycle_phase: 'active',
    operationally_isolated: false,
    agent_protocol_version: AGENT_PROTOCOL_VERSION,
    vpngate_probe_enabled: true,
    vpngate_probe_workers: 16,
    vpngate_intelligence_enabled: false,
    runtime_report_fresh: true,
    runtime_versions: {
      agent: 'agent-build',
      xray: 'Xray 26.4.25',
      phantun: null,
      openvpn: 'OpenVPN 2.6.12 x86_64-pc-linux-gnu',
      vpngate_catalog_probe_workers: 128,
      wg_tools: null,
      wg_backend: null,
    },
  },
  {
    node_id: 'sg-edge',
    name: '新加坡接入',
    lifecycle_phase: 'active',
    operationally_isolated: false,
    agent_protocol_version: AGENT_PROTOCOL_VERSION,
    vpngate_probe_enabled: false,
    vpngate_probe_workers: null,
    vpngate_intelligence_enabled: false,
    runtime_report_fresh: true,
    runtime_versions: {
      agent: 'agent-build',
      xray: 'Xray 26.4.25',
      phantun: null,
      openvpn: null,
      wg_tools: null,
      wg_backend: null,
    },
  },
] as NodeAgentStateItem[];

function mount(
  catalogue: VpngateOverview = overview,
  serverData: VpngateServerView[] = servers,
  runtimeData: VpngateRuntimeView[] = runtimes,
  snapshotValue: ConsoleSnapshot = snapshot,
  nodeData: NodeAgentStateItem[] = nodes,
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData(['snapshot'], snapshotValue);
  client.setQueryData(['tenants'], {
    tenants: [{ id: 'platform', name: '平台', node_count: 1, user_count: 0, operator_count: 1 }],
  });
  client.setQueryData(['vpngate'], catalogue);
  const defaultDirectory = [...serverData].sort(
    (left, right) =>
      (left.candidate_rank == null ? Number.POSITIVE_INFINITY : left.candidate_rank) -
        (right.candidate_rank == null ? Number.POSITIVE_INFINITY : right.candidate_rank) ||
      left.id.localeCompare(right.id),
  );
  client.setQueryData(['vpngate', 'country', 'JP'], {
    items: defaultDirectory.slice(0, 100),
    total: serverData.length,
    page: 1,
    page_size: 100,
  });
  client.setQueryData(['vpngate', 'runtimes'], runtimeData);
  client.setQueryData(['nodes'], { nodes: nodeData });
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          initial,
          who: {
            operator_id: 'root',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
        }}
      >
        <VpngatePage />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

function mountWithoutSnapshot() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData(['tenants'], {
    tenants: [{ id: 'platform', name: '平台', node_count: 1, user_count: 0, operator_count: 1 }],
  });
  client.setQueryData(['vpngate'], overview);
  client.setQueryData(['vpngate', 'country', 'JP'], {
    items: servers,
    total: servers.length,
    page: 1,
    page_size: 100,
  });
  client.setQueryData(['vpngate', 'runtimes'], runtimes);
  vi.spyOn(globalThis, 'fetch').mockImplementation(input => {
    const url = String(input);
    if (url === '/model/snapshot') return new Promise<Response>(() => undefined);
    throw new Error(`unexpected request ${url}`);
  });
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          initial,
          who: {
            operator_id: 'root',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
        }}
      >
        <VpngatePage />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

function mockLegacyDirectory(serverData: VpngateServerView[]) {
  vi.spyOn(globalThis, 'fetch').mockImplementation(async input => {
    const url = String(input);
    if (url.startsWith('/vpngate/countries/JP/servers?')) return new Response(JSON.stringify(serverData));
    throw new Error(`unexpected request ${url}`);
  });
}

beforeEach(() => {
  draft.clear();
  vi.stubGlobal('EventSource', RuntimeEvents);
});
afterEach(() => {
  cleanup();
  draft.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it('follows live main/standby roles and fetches evidence for the new main before switching it', async () => {
  const current: VpngateRuntimeView = {
    ...runtimes[0],
    selected_server_id: 'vpn-jp-2',
    selected_hostname: 'public-vpn-2',
    latest_exit_ip: '198.51.100.22',
    latest_download_bps: 55_000_000,
  };
  const fetch = vi.spyOn(globalThis, 'fetch').mockImplementation(async (input, init) => {
    const url = String(input);
    if (url.endsWith('/switch')) {
      expect(JSON.parse(String(init?.body))).toEqual({ expected_server_id: 'vpn-jp-2' });
      return new Response(JSON.stringify({ request_id: 10, status: 'pending' }), { status: 202 });
    }
    if (url === '/vpngate/runtimes') return new Response(JSON.stringify([current]));
    throw new Error(`unexpected request ${url}`);
  });
  mount();
  act(() => RuntimeEvents.current.sample());
  const entry = document.querySelector('.vpngate-runtime-entry')!;
  expect(entry.querySelector('.vpngate-runtime-candidate')?.textContent).toContain('主用 · 健康public-vpn-2');
  expect(entry.querySelector('.vpngate-runtime-standby')?.textContent).toContain('备用public-vpn-1健康');
  expect(entry.textContent).toContain('主备切换 8 次');
  await waitFor(() => expect(entry.textContent).toContain('198.51.100.22'));
  expect(entry.textContent).not.toContain('候选参考');
  expect(entry.textContent).toContain('55.0 Mbps');
  fireEvent.click(screen.getByRole('button', { name: '切换节点' }));
  expect(screen.getByText(/当前：public-vpn-2/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '确认切换' }));
  await waitFor(() => expect(fetch.mock.calls.some(([input]) => String(input).endsWith('/switch'))).toBe(true));
});

it('labels candidate evidence while the selected machine sample is still missing', () => {
  const missingEvidence: VpngateRuntimeView = {
    ...runtimes[0],
    latest_probe_status: null,
    latest_exit_ip: null,
    latest_exit_country_code: null,
    latest_connect_ms: null,
    latest_download_bps: null,
    latest_ip_scores: [],
    latest_ip_networks: [],
    latest_error_code: null,
    latest_error_detail: null,
    latest_probed_at_unix_secs: null,
    latest_successful_probed_at_unix_secs: null,
    intelligence_verified_at_unix_secs: null,
    intelligence_stale: false,
  };

  mount(overview, servers, [missingEvidence]);

  const entry = document.querySelector('.vpngate-runtime-entry')!;
  expect(entry.textContent).toContain('198.51.100.20');
  expect(entry.textContent).toContain('日本 · 候选参考');
  expect(entry.textContent).toContain('参考拨测');
  expect(entry.textContent).toContain('640 ms');
  expect(entry.textContent).toContain('38.0 Mbps');
});

it('requires a new confirmation if the main changes and disables switching on stale live state', () => {
  vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(JSON.stringify(runtimes)));
  mount();
  act(() => RuntimeEvents.current.sample('vpn-jp-1'));
  fireEvent.click(screen.getByRole('button', { name: '切换节点' }));
  act(() => RuntimeEvents.current.sample('vpn-jp-2'));
  expect(screen.getByRole('button', { name: '确认切换' }).hasAttribute('disabled')).toBe(true);
  expect(screen.getByText('主用节点已变化，请关闭后重新确认。')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '取消' }));
  act(() => RuntimeEvents.current.sample('vpn-jp-2', 20_000));
  expect(screen.getByText('上次主用 · 数据已过期')).toBeTruthy();
  expect(screen.getByRole('button', { name: '切换节点' }).hasAttribute('disabled')).toBe(true);
});

it('keeps VPN Gate in one page with country selection and real node evidence', () => {
  mount();

  expect(screen.getByRole('heading', { name: 'VPN Gate' })).toBeTruthy();
  expect(
    document.querySelector('.vpngate-page.cg-page .vpngate-title-icon.nd-idplate .nd-idplate-clip .geo-flag'),
  ).not.toBeNull();
  const catalogueLamp = screen.getByLabelText('目录正常');
  expect(catalogueLamp.classList.contains('node-lamp')).toBe(true);
  expect(catalogueLamp.closest('.vpngate-title-icon.nd-idplate')).not.toBeNull();
  expect(screen.queryByText('目录正常')).toBeNull();
  expect(document.querySelector('.vpngate-page.cg-page .vpngate-title-icon .detail-title-glyph')).toBeNull();
  expect(screen.queryByText('目录自动同步正常')).toBeNull();
  expect(screen.queryByText(/托管出口池/)).toBeNull();
  expect(screen.queryByText('线路未使用')).toBeNull();
  expect(screen.getByRole('tablist', { name: 'VPN Gate 页面' })).toBeTruthy();
  expect(screen.getByRole('tab', { name: '出口池' }).getAttribute('aria-selected')).toBe('true');
  expect(screen.getByRole('complementary', { name: '地区' })).toBeTruthy();
  const sidePanels = Array.from(document.querySelectorAll('.vpngate-side > .panel'));
  expect(sidePanels.length).toBeGreaterThan(0);
  expect(sidePanels.every(panel => panel.classList.contains('config-panel'))).toBe(true);
  expect(document.querySelector('.vpngate-side .vpngate-country-rail.panel.config-panel.cg-sec')).not.toBeNull();
  expect(document.querySelector('.vpngate-work .vpngate-candidates.panel.titled.cg-sec')).not.toBeNull();
  expect(
    Array.from(document.querySelectorAll('.vpngate-work > section')).map(section =>
      ['vpngate-usage-section', 'vpngate-candidates'].find(name => section.classList.contains(name)),
    ),
  ).toEqual(['vpngate-usage-section', 'vpngate-candidates']);
  expect(document.querySelector('.vpngate-usage-section .panel-title-icon svg')).not.toBeNull();
  expect(document.querySelector('.vpngate-candidates .panel-title-icon svg')).not.toBeNull();
  const countrySummary = document.querySelector('.vpngate-country-summary');
  expect(countrySummary?.classList.contains('config-panel')).toBe(true);
  expect(countrySummary?.textContent).not.toContain('个节点池');
  expect(countrySummary?.textContent).not.toContain('机器出口');
  expect(screen.getByRole('heading', { name: '规则与承载机器' })).toBeTruthy();
  expect(screen.queryByRole('heading', { name: '线路规则中的出站' })).toBeNull();
  expect(screen.getByRole('button', { name: /日本，42 个目录节点/ })).toBeTruthy();
  expect(screen.queryByText('目录拨测机器')).toBeNull();
  expect(screen.getAllByText('香港接入').length).toBeGreaterThan(0);
  expect(screen.getAllByText('198.51.100.20').length).toBeGreaterThan(0);
  expect(screen.getByText('42.0 Mbps')).toBeTruthy();
  expect(screen.getAllByText('全局建连 640 ms')).toHaveLength(12);
  const risk = screen.getAllByText('PC 8 · FF 6 · IL 7')[0];
  expect(risk.getAttribute('title')).toBe('ProxyCheck 8 / 100 · 日本\nFFraud 6 / 100 · 日本\nIPLogs 7 / 100 · 日本');
  expect(screen.getAllByText('商宽').length).toBeGreaterThan(0);
  expect(screen.getAllByText('Example ISP').length).toBeGreaterThan(0);
  expect(screen.getByRole('columnheader', { name: 'VPN Gate 目录数据' })).toBeTruthy();
  expect(screen.getByRole('columnheader', { name: 'Brocade 全局实测' })).toBeTruthy();
  expect(screen.getByRole('columnheader', { name: '线路质量' })).toBeTruthy();
  expect(screen.getByRole('columnheader', { name: '全局质量' })).toBeTruthy();
  expect(screen.getByText('单流性能')).toBeTruthy();
  expect(screen.getByRole('columnheader', { name: '样本' })).toBeTruthy();
  expect(screen.queryByRole('columnheader', { name: '下载' })).toBeNull();
  expect(screen.queryByText('下载')).toBeNull();
  expect(screen.queryByText(/综合质量分|实测质量|质量分/)).toBeNull();
  expect(screen.queryByText('运行中')).toBeNull();
  expect(screen.queryByText('platform')).toBeNull();
  expect(screen.getAllByText('public-vpn-1').length).toBeGreaterThan(0);
  expect(screen.getAllByText(/^候选 #/)).toHaveLength(2);
  expect(screen.getAllByText(/^候补 #/)).toHaveLength(10);

  fireEvent.click(screen.getByRole('tab', { name: '目录采集' }));
  expect(screen.getByRole('heading', { name: 'VPN Gate 目录同步' })).toBeTruthy();
  expect(screen.getByRole('heading', { name: '拨测机器' })).toBeTruthy();
  expect(screen.getByText(/上游目录采集 Agent 在设置「情报任务」中统一选择/)).toBeTruthy();
  expect(screen.getByText(/与设置中的情报执行 Agent 独立/)).toBeTruthy();
  const probeMeta = document.querySelector('.vpngate-probe-section .cg-meta');
  expect(probeMeta?.textContent).toContain('参与 1 台');
  expect(probeMeta?.textContent).toContain('并发 16');
  expect(screen.queryByText(/每台已选 Agent 持续遍历全部历史保留节点/)).toBeNull();
  expect(screen.getByText(/hk-edge · OpenVPN 2\.6\.12/)).toBeTruthy();
  expect(screen.queryByText(/未安装或无法执行 OpenVPN/)).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: '显示不满足条件的 1 台' }));
  expect(screen.getByText(/sg-edge · 未安装或无法执行 OpenVPN/)).toBeTruthy();
  expect((screen.getByRole('combobox', { name: '香港接入 的目录拨测并发数' }) as HTMLSelectElement).value).toBe('16');
  expect(screen.queryByText(/线程/)).toBeNull();
});

it('shows the catalogue while the model snapshot is still loading', () => {
  mountWithoutSnapshot();

  expect(screen.getByRole('heading', { name: 'VPN Gate' })).toBeTruthy();
  expect(screen.getByRole('complementary', { name: '地区' })).toBeTruthy();
  expect(screen.getAllByRole('status', { name: '加载中…' }).length).toBeGreaterThan(0);
  expect(screen.getByText('public-vpn-1')).toBeTruthy();
});

it('shows the page shell and starts the configured country request before overview finishes', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['vpngate', 'runtimes'], runtimes);
  const fetch = vi.spyOn(globalThis, 'fetch').mockImplementation(input => {
    const url = String(input);
    if (url === '/vpngate') return new Promise<Response>(() => undefined);
    if (url.startsWith('/vpngate/countries/JP/servers?')) {
      return Promise.resolve(
        new Response(JSON.stringify({ items: servers, total: servers.length, page: 1, page_size: 100 })),
      );
    }
    throw new Error(`unexpected request ${url}`);
  });

  render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          initial,
          who: {
            operator_id: 'root',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
        }}
      >
        <VpngatePage />
      </SessionProvider>
    </QueryClientProvider>,
  );

  expect(screen.getByRole('heading', { name: 'VPN Gate' })).toBeTruthy();
  expect(screen.getByRole('heading', { name: '日本出口池' })).toBeTruthy();
  expect(screen.getByRole('heading', { name: '规则与承载机器' })).toBeTruthy();
  expect(screen.getByRole('heading', { name: '目录节点' })).toBeTruthy();
  expect(screen.getAllByRole('status', { name: '加载中…' }).length).toBeGreaterThan(0);
  await waitFor(() =>
    expect(fetch.mock.calls.some(([input]) => String(input).startsWith('/vpngate/countries/JP/servers?'))).toBe(true),
  );
});

it('defaults to the country with the most candidates and sorts the rail the same way', () => {
  mount({ ...overview, countries: [...overview.countries].reverse() });

  const sort = screen.getByRole('combobox', { name: '地区排序' }) as HTMLSelectElement;
  expect(sort.value).toBe('candidate');
  const countryButtons = Array.from(document.querySelectorAll<HTMLButtonElement>('.vpngate-country-list > button'));
  expect(countryButtons.map(button => button.getAttribute('aria-label')?.split('，')[0])).toEqual(['日本', '韩国']);
  expect(countryButtons[0].getAttribute('aria-pressed')).toBe('true');
});

it('uses plain-weight empty states for rule references and carrier machines', () => {
  const unreferencedSnapshot: ConsoleSnapshot = {
    ...snapshot,
    snapshot: {
      ...snapshot.snapshot,
      apps: snapshot.snapshot.apps.map(app => ({ ...app, steps: [] })),
    },
  };
  mount(overview, servers, [], unreferencedSnapshot);

  expect(screen.getByText('当前未被线路规则使用，因此没有承载机器').tagName).toBe('SPAN');
});

it('renders every candidate server returned for the selected country', () => {
  mount();

  expect(screen.getByText('public-vpn-12')).toBeTruthy();
  expect(screen.getAllByRole('row')).toHaveLength(14);
});

it('paginates the complete directory at one hundred rows per page', async () => {
  const manyServers = Array.from({ length: 205 }, (_, index): VpngateServerView => ({
    ...servers[0],
    id: `vpn-jp-${String(index + 1).padStart(3, '0')}`,
    hostname: `directory-node-${String(index + 1).padStart(3, '0')}`,
    ip: `192.0.${Math.floor(index / 250) + 2}.${(index % 250) + 1}`,
    active: index < 2,
  }));
  mockLegacyDirectory(manyServers);
  mount(overview, manyServers);

  const tableRows = () => document.querySelectorAll('.vpngate-server-table tbody tr');
  expect(tableRows()).toHaveLength(100);
  expect(screen.getByText('directory-node-100')).toBeTruthy();
  expect(screen.queryByText('directory-node-101')).toBeNull();
  expect(document.querySelector('.vpngate-directory-range')?.textContent).toContain('1–100 / 205');
  expect(document.querySelector('.vpngate-directory-range')?.textContent).toContain('每页 100');

  fireEvent.click(screen.getByRole('button', { name: '下一页' }));
  await waitFor(() => expect(screen.getByText('directory-node-101')).toBeTruthy());
  expect(tableRows()).toHaveLength(100);
  expect(screen.queryByText('directory-node-100')).toBeNull();
  expect(screen.getByText('directory-node-200')).toBeTruthy();
  expect(document.querySelector('.vpngate-directory-range')?.textContent).toContain('101–200 / 205');

  fireEvent.click(screen.getByRole('button', { name: '下一页' }));
  await waitFor(() => expect(screen.getByText('directory-node-205')).toBeTruthy());
  expect(tableRows()).toHaveLength(5);
  expect((screen.getByRole('combobox', { name: '目录节点页码' }) as HTMLSelectElement).value).toBe('3');
  expect((screen.getByRole('button', { name: '下一页' }) as HTMLButtonElement).disabled).toBe(true);
});

it('filters before pagination and follows the explicit backend candidate rank', async () => {
  const sortableServers: VpngateServerView[] = [
    {
      ...servers[0],
      id: 'fast-second',
      hostname: 'fast-second',
      active: true,
      candidate_rank: 2,
      pareto_layer: 1,
      global_download_bps: 80_000_000,
      global_connect_ms: 320,
      latest_download_bps: 80_000_000,
      latest_connect_ms: 320,
    },
    {
      ...servers[0],
      id: 'failed-second',
      hostname: 'failed-second',
      active: false,
      candidate_rank: null,
      pareto_layer: null,
      global_download_bps: null,
      global_connect_ms: null,
      successful_samples: 0,
      latest_probe_status: 'failed',
      latest_download_bps: null,
      latest_connect_ms: null,
      latest_error_code: 'catalogue-probe-failed',
    },
    {
      ...servers[0],
      id: 'candidate-first',
      hostname: 'candidate-first',
      active: true,
      candidate_rank: 1,
      pareto_layer: 1,
      global_download_bps: 10_000_000,
      global_connect_ms: 900,
      latest_download_bps: 10_000_000,
      latest_connect_ms: 900,
      latest_ip_networks: [{ provider: 'proxycheck', isp: 'Special Transit', network_type: 'business' }],
    },
  ];
  mockLegacyDirectory(sortableServers);
  mount(overview, sortableServers);

  const visibleHostnames = () =>
    Array.from(document.querySelectorAll('.vpngate-server-table tbody .vpngate-host > b')).map(
      element => element.textContent,
    );
  expect((screen.getByRole('combobox', { name: '目录节点排序' }) as HTMLSelectElement).value).toBe('candidate');
  expect(visibleHostnames()).toEqual(['candidate-first', 'fast-second', 'failed-second']);

  fireEvent.change(screen.getByRole('combobox', { name: '目录节点排序' }), { target: { value: 'download' } });
  await waitFor(() => expect(visibleHostnames()).toEqual(['fast-second', 'candidate-first', 'failed-second']));

  fireEvent.change(screen.getByRole('combobox', { name: '目录节点筛选' }), { target: { value: 'candidate' } });
  await waitFor(() => expect(visibleHostnames()).toEqual(['fast-second', 'candidate-first']));

  fireEvent.change(screen.getByRole('combobox', { name: '目录节点筛选' }), { target: { value: 'all' } });
  fireEvent.change(screen.getByRole('searchbox', { name: '搜索目录节点' }), {
    target: { value: 'special transit' },
  });
  await waitFor(() => expect(visibleHostnames()).toEqual(['candidate-first']));

  fireEvent.change(screen.getByRole('searchbox', { name: '搜索目录节点' }), { target: { value: 'not-found' } });
  await screen.findByText('没有匹配的目录节点');
  fireEvent.click(screen.getByRole('button', { name: '清除筛选' }));
  await waitFor(() => expect(visibleHostnames()).toEqual(['candidate-first', 'fast-second', 'failed-second']));
});

it('distinguishes priority review from suspended VPN Gate candidates', async () => {
  const nowUnixSecs = Math.floor(Date.now() / 1000);
  const reviewServers: VpngateServerView[] = [
    {
      ...servers[0],
      id: 'reviewing-node',
      hostname: 'reviewing-node',
      active: true,
      candidate_rank: 1,
      consecutive_probe_failures: 2,
      probe_eligible_until_unix_secs: nowUnixSecs + 1_200,
      latest_probe_status: 'failed',
      latest_error_code: 'catalogue-probe-failed',
    },
    {
      ...servers[0],
      id: 'expired-review-node',
      hostname: 'expired-review-node',
      active: false,
      candidate_rank: null,
      pareto_layer: null,
      consecutive_probe_failures: 2,
      probe_eligible_until_unix_secs: nowUnixSecs - 1,
      latest_probe_status: 'failed',
      latest_error_code: 'catalogue-probe-failed',
    },
    {
      ...servers[0],
      id: 'suspended-node',
      hostname: 'suspended-node',
      active: false,
      candidate_rank: null,
      pareto_layer: null,
      consecutive_probe_failures: 3,
      probe_eligible_until_unix_secs: nowUnixSecs + 1_200,
      latest_probe_status: 'failed',
      latest_error_code: 'catalogue-probe-failed',
    },
  ];
  mockLegacyDirectory(reviewServers);
  mount(overview, reviewServers);

  expect(screen.getByText('候选 #1 · 复核')).toBeTruthy();
  expect(screen.getByText('复核 2/3')).toBeTruthy();
  expect(screen.getByText(/复核窗口至/)).toBeTruthy();
  expect(screen.queryByText('暂停候选')).toBeNull();
  expect(screen.getAllByText('已暂停')).toHaveLength(2);
  expect(screen.getByText(/20 分钟复核窗口已结束/)).toBeTruthy();
  expect(screen.getByText(/连续 3 次失败/)).toBeTruthy();

  const visibleHostnames = () =>
    Array.from(document.querySelectorAll('.vpngate-server-table tbody .vpngate-host > b')).map(
      element => element.textContent,
    );
  fireEvent.change(screen.getByRole('combobox', { name: '目录节点筛选' }), { target: { value: 'reviewing' } });
  await waitFor(() => expect(visibleHostnames()).toEqual(['reviewing-node']));
  fireEvent.change(screen.getByRole('combobox', { name: '目录节点筛选' }), { target: { value: 'suspended' } });
  await waitFor(() => expect(visibleHostnames()).toEqual(['expired-review-node', 'suspended-node']));
});

it('keeps the last successful exit intelligence visible after a newer dial failure', () => {
  const failedServer = {
    ...servers[0],
    latest_probe_status: 'failed',
    latest_error_code: 'openvpn-connect',
    latest_probed_at_unix_secs: 1_800_000_000,
  };
  const failedRuntime = {
    ...runtimes[0],
    runtime_status: 'degraded',
    latest_probe_status: 'failed',
    latest_error_code: 'openvpn-connect',
    latest_error_detail: 'connection timed out',
    latest_probed_at_unix_secs: 1_800_000_000,
  };
  mount(overview, [failedServer], [failedRuntime]);

  expect(screen.getAllByText('198.51.100.20').length).toBeGreaterThan(0);
  expect(screen.getAllByText(/上次成功情报/).length).toBeGreaterThan(0);
  expect(screen.getAllByText(/openvpn-connect/).length).toBeGreaterThan(0);
  expect(screen.queryByText('等待拨测结果')).toBeNull();
});

it('describes missing VPN Gate measurements without calling them intelligence', () => {
  const unmeasuredServer = {
    ...servers[0],
    latest_ip_scores: [],
    latest_ip_networks: [],
  };
  const unmeasuredRuntime = {
    ...runtimes[0],
    latest_ip_scores: [],
    latest_ip_networks: [],
  };
  mount(overview, [unmeasuredServer], [unmeasuredRuntime]);

  expect(screen.getAllByText('等待拨测结果').length).toBeGreaterThan(0);
  expect(screen.getAllByText('尚未拨测成功').length).toBeGreaterThan(0);
  expect(screen.queryByText('等待 IP 情报')).toBeNull();
  expect(screen.queryByText('等待网络情报')).toBeNull();
});

it('does not attach an obsolete same-country runtime to a newly referenced pool', () => {
  const obsoleteRuntime: VpngateRuntimeView = {
    ...runtimes[0],
    node_id: 'retired-edge',
    node_name: '旧承载机器',
    outbound_id: 'vpngate-bbbb-bbbb',
    outbound_name: '旧日本池',
  };
  mount(overview, servers, [...runtimes, obsoleteRuntime]);

  expect(screen.queryByText('1 个节点池 · 1 台机器')).toBeNull();
  expect(screen.queryByText(/台可运行 OpenVPN/)).toBeNull();
  expect(screen.queryByText('旧承载机器')).toBeNull();
});

it('queues a manual automatic-pool switch and explains the ten-minute cooldown', async () => {
  const fetch = vi.spyOn(globalThis, 'fetch').mockImplementation(async (input, init) => {
    const url = String(input);
    if (url.includes('/vpngate/runtimes/hk-edge/vpngate-aaaa-aaaa/switch')) {
      expect(init?.method).toBe('POST');
      expect(JSON.parse(String(init?.body))).toEqual({ expected_server_id: 'vpn-jp-1' });
      return new Response(
        JSON.stringify({
          request_id: 9,
          node_id: 'hk-edge',
          outbound_id: 'vpngate-aaaa-aaaa',
          previous_server_id: 'vpn-jp-1',
          status: 'pending',
          selected_server_id: null,
          cooldown_until_unix_secs: null,
          error_detail: null,
          requested_at_unix_secs: 1_800_000_000,
          completed_at_unix_secs: null,
        }),
        { status: 202, headers: { 'content-type': 'application/json' } },
      );
    }
    if (url.includes('/vpngate/runtimes')) {
      return new Response(JSON.stringify(runtimes), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }
    throw new Error(`unexpected request ${url}`);
  });
  mount();

  const switchButton = screen.getByRole('button', { name: '切换节点' });
  expect(switchButton.classList.contains('primary')).toBe(true);
  fireEvent.click(switchButton);
  expect(screen.getByText('切换 VPN Gate 节点')).toBeTruthy();
  expect(screen.getByText(/成功后旧节点冷却 10 分钟/)).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '确认切换' }));

  await waitFor(() =>
    expect(fetch.mock.calls.some(([input]) => String(input).includes('/vpngate-aaaa-aaaa/switch'))).toBe(true),
  );
});

it('shows real OpenVPN timeouts and sorts incomplete connection measurements after healthy runtimes', () => {
  const missingMeasurement: VpngateRuntimeView = {
    ...runtimes[0],
    node_id: 'missing-connect',
    node_name: '待更新机器',
    latest_connect_ms: 0,
  };
  const timedOut: VpngateRuntimeView = {
    ...runtimes[0],
    node_id: 'timed-out',
    node_name: '超时机器',
    runtime_status: 'degraded',
    latest_probe_status: 'failed',
    latest_connect_ms: null,
    latest_error_code: 'runtime-start-failed',
    latest_error_detail: 'OpenVPN initialization timed out',
  };
  mount(overview, servers, [timedOut, missingMeasurement, runtimes[0]]);

  const rows = Array.from(document.querySelectorAll('.vpngate-runtime-list .vpngate-runtime-row'));
  expect(rows.map(row => row.querySelector('.vpngate-runtime-machine b')?.textContent)).toEqual([
    '香港接入',
    '待更新机器',
    '超时机器',
  ]);
  expect(screen.getByText('待更新')).toBeTruthy();
  expect(screen.getByText('超时 35 秒')).toBeTruthy();
  expect(screen.queryByText('0 ms')).toBeNull();
});

it('shows the previous node cooldown without extra explanatory copy', () => {
  mount(overview, servers, [
    {
      ...runtimes[0],
      switch_request_id: 9,
      switch_status: 'applied',
      switch_previous_server_id: 'vpn-jp-1',
      switch_previous_hostname: 'public-vpn-1',
      switch_selected_server_id: 'vpn-jp-2',
      switch_cooldown_until_unix_secs: Math.floor(Date.now() / 1_000) + 600,
    },
  ]);

  expect(screen.getByText(/public-vpn-1冷却 \d{2}:\d{2}/)).toBeTruthy();
});

it('lets a system administrator change the selected catalogue probe subset', async () => {
  const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
    new Response(
      JSON.stringify({
        node_id: 'hk-edge',
        enabled: false,
        workers: null,
        selected_at_unix_secs: null,
      }),
      { status: 200, headers: { 'content-type': 'application/json' } },
    ),
  );
  mount();
  fireEvent.click(screen.getByRole('tab', { name: '目录采集' }));
  fireEvent.click(screen.getByRole('button', { name: '停用 香港接入 的 VPN Gate 目录拨测' }));

  await waitFor(() => expect(screen.getByRole('button', { name: '启用 香港接入 的 VPN Gate 目录拨测' })).toBeTruthy());
  expect(fetch).toHaveBeenCalledWith(
    '/vpngate/probe-nodes/hk-edge',
    expect.objectContaining({ method: 'PUT', body: JSON.stringify({ enabled: false, workers: 16 }) }),
  );
});

it('lets a system administrator change catalogue probe concurrency from the selected machine menu', async () => {
  const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
    new Response(
      JSON.stringify({
        node_id: 'hk-edge',
        enabled: true,
        workers: 8,
        selected_at_unix_secs: 1_800_000_000,
      }),
      { status: 200, headers: { 'content-type': 'application/json' } },
    ),
  );
  mount();
  fireEvent.click(screen.getByRole('tab', { name: '目录采集' }));
  fireEvent.change(screen.getByRole('combobox', { name: '香港接入 的目录拨测并发数' }), {
    target: { value: '8' },
  });

  await waitFor(() =>
    expect(fetch).toHaveBeenCalledWith(
      '/vpngate/probe-nodes/hk-edge',
      expect.objectContaining({ method: 'PUT', body: JSON.stringify({ enabled: true, workers: 8 }) }),
    ),
  );
  await waitFor(() =>
    expect((screen.getByRole('combobox', { name: '香港接入 的目录拨测并发数' }) as HTMLSelectElement).value).toBe('8'),
  );
  expect(screen.queryByText(/线程/)).toBeNull();
});

it('saves connectivity, performance, and shard schedules for the next batch', async () => {
  const updatedStatus = {
    ...overview.status,
    probe_success_cooldown_secs: 3600,
    probe_performance_cooldown_secs: 43200,
    probe_shard_rotation_secs: 43200,
  };
  const fetch = vi.spyOn(globalThis, 'fetch').mockImplementation(async (input, init) => {
    const url = String(input);
    if (url === '/vpngate/probe-settings') {
      expect(init?.method).toBe('PUT');
      expect(JSON.parse(String(init?.body))).toEqual({
        success_cooldown_secs: 3600,
        performance_cooldown_secs: 43200,
        shard_rotation_secs: 43200,
      });
      return new Response(JSON.stringify(updatedStatus), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }
    if (url === '/vpngate') {
      return new Response(JSON.stringify({ ...overview, status: updatedStatus }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }
    throw new Error(`unexpected request ${url}`);
  });
  mount();
  fireEvent.click(screen.getByRole('tab', { name: '目录采集' }));

  const connectivity = screen.getByRole('spinbutton', { name: '连通性复测间隔（分钟）' });
  const performance = screen.getByRole('spinbutton', { name: '单流性能复测间隔（小时）' });
  const rotation = screen.getByRole('spinbutton', { name: '拨测节点轮转周期（小时）' });
  expect((connectivity as HTMLInputElement).value).toBe('30');
  expect((performance as HTMLInputElement).value).toBe('6');
  expect((rotation as HTMLInputElement).value).toBe('6');
  expect(screen.getByText(/连通性与单流性能分开调度/)).toBeTruthy();

  fireEvent.change(connectivity, { target: { value: '60' } });
  fireEvent.change(performance, { target: { value: '12' } });
  fireEvent.change(rotation, { target: { value: '12' } });
  fireEvent.click(screen.getByRole('button', { name: '保存目录拨测设置' }));

  await waitFor(() => expect(fetch).toHaveBeenCalledWith('/vpngate/probe-settings', expect.anything()));
  await waitFor(() => expect((connectivity as HTMLInputElement).value).toBe('60'));
  expect((performance as HTMLInputElement).value).toBe('12');
  expect((rotation as HTMLInputElement).value).toBe('12');
});

it('lists selected catalogue probe machines first with a compact concurrency menu', () => {
  mount(overview, servers, runtimes, snapshot, [
    {
      ...nodes[0],
      name: '已选机器',
    },
    {
      ...nodes[0],
      node_id: 'available-a',
      name: '可用机器',
      vpngate_probe_enabled: false,
      vpngate_probe_workers: null,
    },
    nodes[1],
  ]);
  fireEvent.click(screen.getByRole('tab', { name: '目录采集' }));
  let cards = Array.from(document.querySelectorAll('.vpngate-capability-list article'));
  expect(cards.map(card => card.querySelector('b')?.textContent)).toEqual(['已选机器', '可用机器']);
  fireEvent.click(screen.getByRole('button', { name: '显示不满足条件的 1 台' }));
  cards = Array.from(document.querySelectorAll('.vpngate-capability-list article'));
  expect(cards.map(card => card.querySelector('b')?.textContent)).toEqual(['已选机器', '可用机器', '新加坡接入']);
  expect(cards[0].classList.contains('selected')).toBe(true);
  expect(document.querySelector('.vpngate-capability-state')).toBeNull();
  expect(screen.queryByText('已选 · 1 个出口池')).toBeNull();
  expect(document.querySelectorAll('.vpngate-capability-list select')).toHaveLength(1);
  expect(screen.getByRole('combobox', { name: '已选机器 的目录拨测并发数' })).toBeTruthy();
  expect(screen.queryByText(/线程/)).toBeNull();
});

it('warns on a selected catalogue probe machine whose accepted data is stale', () => {
  mount(overview, servers, runtimes, snapshot, [
    {
      ...nodes[0],
      vpngate_probe_reported_at: '2027-01-15T01:02:03Z',
      vpngate_probe_data_stale: true,
    },
  ]);
  fireEvent.click(screen.getByRole('tab', { name: '目录采集' }));

  const warning = screen.getByRole('img', { name: /香港接入：拨测长时间无有效数据更新/ });
  expect(warning.textContent).toBe('!');
  expect(warning.getAttribute('title')).toContain('最后有效数据');
  expect(warning.closest('article')?.classList.contains('warn')).toBe(true);
  expect(document.querySelector('.vpngate-probe-section .cg-meta .warn')?.textContent).toBe('1');
});

it('keeps the tunnel page for evidence and collection, without registering rule targets', () => {
  mount();
  expect(screen.queryByRole('button', { name: '配置' })).toBeNull();
  expect(screen.queryByText('配置当前出站')).toBeNull();
  expect(screen.queryByText('调整当前出站')).toBeNull();
  expect(screen.getAllByText('public-vpn-1').length).toBeGreaterThan(0);
  expect(screen.queryByRole('button', { name: '固定使用 public-vpn-1' })).toBeNull();
  expect(draft.ops()).toEqual([]);
});

it('shows only VPN Gate pools referenced by the effective rule model', () => {
  mount();

  expect(screen.getByText('规则与承载机器')).toBeTruthy();
  expect(screen.queryByText(/仅统计当前线路规则实际引用/)).toBeNull();
  expect(screen.getByText('日本规则池')).toBeTruthy();
  const poolSummary = screen.getByText('2 条规则 · 1 台机器');
  expect(poolSummary.classList.contains('vpngate-pool-summary')).toBe(true);
  expect(poolSummary.classList.contains('st')).toBe(false);
  expect(screen.queryByText('遗留未引用池')).toBeNull();
  expect(screen.getByTitle('线路已使用 1 个节点池')).toBeTruthy();
});

it('searches the persistent country rail and keeps collection global', async () => {
  mount();
  fireEvent.change(screen.getByRole('searchbox', { name: '搜索地区或代码' }), { target: { value: '韩国' } });
  expect(screen.getByRole('button', { name: /韩国，21 个目录节点/ })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /日本，42 个目录节点/ })).toBeNull();

  fireEvent.click(screen.getByRole('tab', { name: '目录采集' }));
  expect(screen.getByRole('tab', { name: '目录采集' }).getAttribute('aria-selected')).toBe('true');
  expect(screen.getByRole('heading', { name: 'VPN Gate 目录同步' })).toBeTruthy();
  expect(screen.queryByRole('complementary', { name: '地区' })).toBeNull();
  expect(document.querySelector('.vpngate-collection-sync .cg-meta')?.textContent).toContain('快照 27');
  expect(document.querySelector('.vpngate-collection-sync .cg-meta')?.textContent).toContain('目录 63');
  expect(document.querySelector('.vpngate-collection-sync .cg-meta')?.textContent).toContain('观测 412');
  expect(document.querySelector('.vpngate-collection-sync .cg-meta')?.textContent).toContain('本轮首次发现 3');
  expect(screen.getByRole('img', { name: '最近 3 次成功同步的快照节点数：21 到 27' })).toBeTruthy();
  expect(screen.getByRole('heading', { name: '拨测机器' })).toBeTruthy();
  expect(screen.queryByText('采集信息')).toBeNull();
  await waitFor(() => {
    expect(chartMocks.init).toHaveBeenCalled();
    expect(chartMocks.setOption).toHaveBeenCalledWith(
      expect.objectContaining({ series: [expect.objectContaining({ type: 'line' })] }),
      true,
    );
  });
});

it('sorts countries by independent available and admitted candidate counts and hides ZZ', () => {
  mount({
    ...overview,
    countries: [
      { ...overview.countries[0], country_code: 'CA', measured_successful: 1, candidate_servers: 5 },
      { ...overview.countries[0], country_code: 'HK', measured_successful: 3, candidate_servers: 5 },
      { ...overview.countries[0], country_code: 'JP', measured_successful: 8, candidate_servers: 2 },
      { ...overview.countries[1], country_code: 'KR', measured_successful: 3, candidate_servers: 7 },
      { ...overview.countries[1], country_code: 'SG', measured_successful: 3, candidate_servers: 1 },
      {
        country_code: 'ZZ',
        country_name: 'Reserved',
        current_servers: 1,
        retained_servers: 1,
        measured_successful: 9,
        candidate_servers: 9,
      },
    ],
  });
  const countryCodes = () =>
    Array.from(document.querySelectorAll('.vpngate-country-list > button .vpngate-country-name small')).map(
      element => element.textContent,
    );

  expect((screen.getByRole('combobox', { name: '地区排序' }) as HTMLSelectElement).value).toBe('candidate');
  expect(countryCodes()).toEqual(['KR', 'HK', 'CA', 'JP', 'SG']);
  expect(screen.queryByText('ZZ')).toBeNull();

  fireEvent.change(screen.getByRole('combobox', { name: '地区排序' }), { target: { value: 'available' } });
  expect(countryCodes()).toEqual(['JP', 'HK', 'KR', 'SG', 'CA']);

  fireEvent.change(screen.getByRole('combobox', { name: '地区排序' }), { target: { value: 'candidate' } });
  expect(countryCodes()).toEqual(['KR', 'HK', 'CA', 'JP', 'SG']);

  fireEvent.change(screen.getByRole('combobox', { name: '地区排序' }), { target: { value: 'region' } });
  expect(countryCodes()).toEqual(['CA', 'HK', 'JP', 'KR', 'SG']);
});

it('keeps the page mounted while a newly selected country loads its evidence', async () => {
  let finishRequest: ((response: Response) => void) | undefined;
  vi.spyOn(globalThis, 'fetch').mockImplementation(() => new Promise<Response>(resolve => (finishRequest = resolve)));
  mount();

  const page = document.querySelector('.vpngate-page');
  const candidates = document.querySelector('.vpngate-candidates');
  const table = document.querySelector('.vpngate-server-table');
  fireEvent.click(screen.getByRole('button', { name: /韩国，21 个目录节点/ }));
  expect(screen.getByRole('heading', { name: '韩国出口池' })).toBeTruthy();
  expect(screen.getByRole('heading', { name: 'VPN Gate' })).toBeTruthy();
  expect(document.querySelector('.vpngate-page')).toBe(page);
  expect(document.querySelector('.vpngate-candidates')).toBe(candidates);
  expect(document.querySelector('.vpngate-server-table')).toBe(table);
  expect(screen.getByText('public-vpn-1')).toBeTruthy();
  expect(candidates?.textContent).toContain('加载中…');
  expect(candidates?.getAttribute('aria-busy')).toBe('true');

  finishRequest?.(new Response(JSON.stringify([]), { status: 200, headers: { 'content-type': 'application/json' } }));
  await waitFor(() => expect(screen.getByText('这个地区还没有目录节点')).toBeTruthy());
});
