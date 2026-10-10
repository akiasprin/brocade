/* 会改机器产物的设置段写草稿；端口默认值与端到端探测设置直接写库。`pristine` 必须同时
 * 合并草稿基准和这两个已提交分段，否则后续保存会把其中一边覆盖回旧值。
 *
 * 与机器详情页那三处是同一个根因（见 draft-discard-refresh.test.tsx），但症状不同：
 * 表单里的值不会回落（TanStack 的结构共享让 `settings.data` 引用不变，重填分支不触发），
 * 变的是「这一段有没有未保存的改动」这个判断——它永远为真。
 *
 * 取数路径必须是真的：stub 的是 `fetch`，不是预置 `setQueryData`。 */
import { useEffect, useState } from 'react';
import { QueryClient, QueryClientProvider, useQueryClient } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AGENT_PROTOCOL_VERSION, type CertsView } from '../src/api';

/* settings.tsx 经 ui/branding → … 在模块求值期读 matchMedia，jsdom 没有实现。
   import 是提升的，所以装在这里而不是 beforeEach。 */
window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener: () => {},
  removeEventListener: () => {},
  addListener: () => {},
  removeListener: () => {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const { draft } = await import('../src/draft');
const { SessionProvider } = await import('../src/session');
const { SettingsPane } = await import('../src/panes/settings');

const SESSION = {
  initial: { node_count: 0, chain_group_count: [] },
  who: {
    operator_id: 'tester',
    role: 'system-admin' as const,
    tenant_scope: null,
    token_prefix: null,
    masked_assets: false,
  },
};

/** 已提交的全局设置。机器相关分段写草稿时这一份不变。 */
const committedSettings = () => ({
  reality_client: { min_client_ver: null, max_client_ver: null, max_time_diff_ms: null },
  reality_site: {
    dest: 'www.committed.example:443',
    server_names: ['www.committed.example'],
    fingerprint: 'chrome',
    flow: 'xtls-rprx-vision',
  },
  overlay: { keepalive_secs: 25, mtu: 1420, disabled_links: [] },
  ports: { ingress_base: 13443, anytls_base: 14443, hop_base: 20000, hy2_base: 30000 },
  probe: { endpoint_url: 'http://cp.cloudflare.com/cdn-cgi/trace', timeout_secs: 10, interval_secs: 60 },
  geodata: { cron: 'CRON_TZ=Asia/Shanghai 30 6 * * *', geoip_url: 'https://geoip', geosite_url: 'https://geosite' },
  connection: {
    conn_idle_secs: 300,
    uplink_only_secs: 2,
    downlink_only_secs: 5,
    buffer_size_kb: null,
    handshake_secs: 60,
  },
  relay_mux: {
    concurrency: 1,
    prewarm_workers: 0,
    reuse_threshold: 2,
    max_probing_workers: 1,
    probe_interval_ms: 5000,
    probe_timeout_ms: 2000,
    idle_ttl_ms: 24000,
    max_requests_per_worker: 128,
  },
  anytls_padding_scheme: ['stop=4'],
  stats_user_online: false,
});

const ROUTES: Record<string, () => unknown> = {
  '/settings': committedSettings,
  '/branding': () => ({ site_name: 'brocade', icon: null, accent: null }),
  '/auth/state': () => ({ public_open: false }),
  '/certs': () => ({ groups: [], nodes: [], letsencrypt: 'https://acme', letsencrypt_staging: 'https://acme-staging' }),
  '/distribution': () => ({
    stored: { agent_public_url: 'https://example', xray_version: '26.7.28' },
    effective: { agent_public_url: 'https://example', xray_version: '26.7.28' },
  }),
  '/agent-log-policy': () => ({
    global: { agent_journal_mib: 100, xray_mib: 100, phantun_mib: 100 },
    nodes: [],
  }),
  '/host-network-tuning': () => ({ gro_flush_timeout_ns: 20000, napi_defer_hard_irqs: 2 }),
  '/tunnel-probes': () => ({
    origin: 'console',
    endpoint_url: 'http://cp.cloudflare.com/cdn-cgi/trace',
    retention_days: 7,
    items: [],
  }),
  '/tunnel-probes/capability': () => ({
    available: true,
    version: 'test',
    reason: null,
    concurrency: 1,
  }),
  '/ping-probe/settings': () => ({ targets: [], interval_secs: 60, timeout_ms: 420 }),
  '/links/mtu': () => ({ default_mtu: 1420, nodes: [], links: [] }),
  '/nodes/agent-state': () => ({ nodes: [] }),
  '/vpngate': () => ({
    manual_pools_supported: true,
    status: {},
    countries: [],
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
    intelligence_credentials: {
      proxycheck_api_key_configured: true,
    },
  }),
  '/revisions?limit=50': () => ({ current_revision: 7, revisions: [{ id: 7 }] }),
};

const immediateSettingsWrites: { path: string; body: unknown }[] = [];
let immediateRevision = 8;

const certsWithGroup = (): CertsView => ({
  sealing_available: true,
  domain: {
    id: 'public.example',
    domain: 'public.example',
    dns_provider: 'cloudflare',
    acme_directory: 'https://acme',
    signing_method: 'public-ca',
    acme_contact: null,
    renew_before_days: 30,
    has_credential: true,
    has_account: false,
  },
  groups: [
    {
      id: 'group-1',
      domain: 'private.example',
      label: 'a1b2c3d4',
      name: 'CA-1',
      signing_method: 'self-signed',
      is_default: false,
      note: null,
      status: 'active',
      names: ['*.a1b2c3d4.private.example', 'a1b2c3d4.private.example'],
      nodes: [],
      certificates: [],
    },
  ],
  nodes: [],
  scan: null,
  letsencrypt: 'https://acme',
  letsencrypt_staging: 'https://acme-staging',
});

const publicCaCertsWithGroup = (): CertsView => {
  const view = certsWithGroup();
  view.domain = {
    ...view.domain!,
    domain: 'example.com',
    acme_directory: 'https://acme',
    signing_method: 'public-ca',
    has_credential: true,
    has_account: true,
  };
  view.groups[0].domain = 'example.com';
  view.groups[0].signing_method = 'public-ca';
  return view;
};

function stubFetch() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      if (path.startsWith('/vpngate/intelligence-nodes/') && init?.method === 'PUT') {
        const nodeId = decodeURIComponent(path.slice('/vpngate/intelligence-nodes/'.length));
        const enabled = Boolean(JSON.parse(String(init.body)).enabled);
        return Response.json({ node_id: nodeId, enabled, selected_at_unix_secs: enabled ? 1 : null });
      }
      if (path === '/vpngate/admission-policy' && init?.method === 'PUT') {
        return Response.json(JSON.parse(String(init.body)));
      }
      if (path === '/vpngate/intelligence-policy' && init?.method === 'PUT') {
        return Response.json(JSON.parse(String(init.body)));
      }
      if (path === '/vpngate/intelligence-credentials' && init?.method === 'PUT') {
        return Response.json({ proxycheck_api_key_configured: true });
      }
      if (path === '/vpngate/intelligence-refresh' && init?.method === 'POST') {
        return Response.json({ queued: 2 });
      }
      if (path === '/settings/ports' || path === '/settings/probe') {
        const body = JSON.parse(String(init?.body));
        const current = ROUTES['/settings']() as ReturnType<typeof committedSettings>;
        const key = path === '/settings/ports' ? 'ports' : 'probe';
        const next = { ...current, [key]: body } as ReturnType<typeof committedSettings>;
        ROUTES['/settings'] = () => next;
        immediateSettingsWrites.push({ path, body });
        return Response.json({ revision_id: immediateRevision++, settings: next });
      }
      const body = ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return new Response(JSON.stringify(body()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

/* 复刻 forge/shell.tsx 的集中失效器：草稿任何变动都失效这两个键。生产里它始终挂载。 */
function ShellDraftInvalidation() {
  const qc = useQueryClient();
  useEffect(
    () =>
      draft.subscribe(() => {
        qc.invalidateQueries({ queryKey: ['snapshot'] });
        qc.invalidateQueries({ queryKey: ['compile'] });
      }),
    [qc],
  );
  return null;
}

function Harness({ role = 'system-admin' }: { role?: 'system-admin' | 'editor' }) {
  const [client] = useState(
    () => new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } }),
  );
  return (
    <QueryClientProvider client={client}>
      <SessionProvider value={{ initial: SESSION.initial, who: { ...SESSION.who, role } }}>
        <ShellDraftInvalidation />
        <SettingsPane />
      </SessionProvider>
    </QueryClientProvider>
  );
}

/** 设置段。状态与保存动作位于段尾，查询范围应覆盖整张卡片。 */
const section = (id: string) => {
  const section = document.getElementById(id);
  if (!section) throw new Error(`没有找到设置段 ${id}`);
  return within(section);
};

const fieldInput = (scope: ReturnType<typeof section>, label: string) => {
  const input = scope.getByText(label, { selector: 'label' }).closest('.setfld')?.querySelector('input');
  if (!(input instanceof HTMLInputElement)) throw new Error(`没有找到字段 ${label}`);
  return input;
};

const settingsOp = () => draft.ops().find(op => op.op === 'update_settings');

beforeEach(() => {
  draft.init(`settings-baseline-${Math.random()}`);
  draft.clear();
  ROUTES['/settings'] = committedSettings;
  immediateSettingsWrites.length = 0;
  immediateRevision = 8;
  stubFetch();
});

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
  ROUTES['/settings'] = committedSettings;
  ROUTES['/links/mtu'] = () => ({ default_mtu: 1420, nodes: [], links: [] });
  ROUTES['/nodes/agent-state'] = () => ({ nodes: [] });
  ROUTES['/certs'] = () => ({
    groups: [],
    nodes: [],
    letsencrypt: 'https://acme',
    letsencrypt_staging: 'https://acme-staging',
  });
});

describe('设置页分段保存的基准', () => {
  it('按来源分别保存 VPN Gate 准入阈值，不提交聚合分数', async () => {
    render(<Harness />);
    await screen.findByText('情报任务');
    const scope = section('set-vpngate-intelligence');
    const ffraud = fieldInput(scope, 'FFraud');
    fireEvent.change(ffraud, { target: { value: '67' } });
    fireEvent.click(scope.getByRole('button', { name: '保存准入规则' }));

    await waitFor(() =>
      expect(vi.mocked(fetch)).toHaveBeenCalledWith(
        '/vpngate/admission-policy',
        expect.objectContaining({
          method: 'PUT',
          body: expect.stringContaining('"provider":"ffraud","maximum_score":67'),
        }),
      ),
    );
    const request = vi.mocked(fetch).mock.calls.find(([path]) => path === '/vpngate/admission-policy');
    expect(request?.[1]?.body).not.toContain('max_ip_risk');
  });

  it('只在定期模式显示刷新周期，并按 72 小时条件保存情报更新规则', async () => {
    render(<Harness />);
    await screen.findByText('情报任务');
    const scope = section('set-vpngate-intelligence');
    const activeWindow = scope.getByRole('spinbutton', {
      name: '纳入 IP 情报更新的最后成功拨通小时数',
    });
    expect((activeWindow as HTMLInputElement).value).toBe('72');
    expect(scope.queryByRole('spinbutton', { name: 'IP 情报刷新周期间隔' })).toBeNull();

    fireEvent.click(scope.getByRole('button', { name: '定期刷新' }));
    expect(scope.getByRole('spinbutton', { name: 'IP 情报刷新周期间隔' })).toBeTruthy();
    fireEvent.change(activeWindow, { target: { value: '48' } });
    fireEvent.click(scope.getByRole('button', { name: '保存更新规则' }));

    await waitFor(() =>
      expect(vi.mocked(fetch)).toHaveBeenCalledWith(
        '/vpngate/intelligence-policy',
        expect.objectContaining({
          method: 'PUT',
          body: expect.stringContaining('"active_window_hours":48'),
        }),
      ),
    );
  });

  it('以不触发密码保存的密钥输入追加 ProxyCheck Key 池，不从 API 读回密钥', async () => {
    render(<Harness />);
    await screen.findByText('情报任务');
    const scope = section('set-vpngate-intelligence');
    const first = scope.getByLabelText('ProxyCheck API 密钥 1') as HTMLInputElement;
    const row = first.closest('.setfld');
    expect(row).not.toBeNull();
    expect(first.type).toBe('text');
    expect(first.autocomplete).toBe('off');
    expect(first.classList.contains('config-secret-input')).toBe(true);
    expect(first.placeholder).toContain('追加');
    expect(
      within(row as HTMLElement).getByText('已配置；旧 Key 不回显，追加不会覆盖 · 最多 32 个，随机起点轮换'),
    ).toBeTruthy();

    const replacements = ['111111-222222-333333-444444', 'aaaaaa-bbbbbb-cccccc-dddddd'];
    fireEvent.change(first, { target: { value: replacements[0] } });
    fireEvent.click(within(row as HTMLElement).getByRole('button', { name: '＋ 添加密钥' }));
    const second = scope.getByLabelText('ProxyCheck API 密钥 2') as HTMLInputElement;
    expect(second.type).toBe('text');
    fireEvent.change(second, { target: { value: replacements[1] } });
    fireEvent.click(within(row as HTMLElement).getByRole('button', { name: '追加到 Key 池' }));

    await waitFor(() => expect((scope.getByLabelText('ProxyCheck API 密钥 1') as HTMLInputElement).value).toBe(''));
    expect(scope.queryByLabelText('ProxyCheck API 密钥 2')).toBeNull();
    const request = vi.mocked(fetch).mock.calls.find(([path]) => path === '/vpngate/intelligence-credentials');
    expect(request?.[1]?.method).toBe('PUT');
    expect(request?.[1]?.body).toBe(JSON.stringify({ proxycheck_api_keys: replacements, mode: 'append' }));
  });

  it('可从全机队多选 Agent 分发目录采集与三源 IP 情报任务', async () => {
    ROUTES['/nodes/agent-state'] = () => ({
      nodes: [
        {
          node_id: 'edge-1',
          tenant_id: 'platform',
          name: '香港出口',
          lifecycle_phase: 'active',
          operationally_isolated: false,
          agent_protocol_version: AGENT_PROTOCOL_VERSION,
          runtime_report_fresh: true,
          vpngate_intelligence_enabled: false,
        },
      ],
    });
    render(<Harness />);
    await screen.findByText('情报执行 Agent');
    expect(screen.getByText(/同时执行 VPN Gate 上游目录采集和出口 IP 情报查询/)).toBeTruthy();
    const checkbox = await screen.findByRole('checkbox', { name: /香港出口/ });
    fireEvent.click(checkbox);
    await waitFor(() => expect((checkbox as HTMLInputElement).checked).toBe(true));
    expect(vi.mocked(fetch)).toHaveBeenCalledWith(
      '/vpngate/intelligence-nodes/edge-1',
      expect.objectContaining({ method: 'PUT', body: JSON.stringify({ enabled: true }) }),
    );
  });

  it('所有设置段与机器配置、链路设置共用 config-panel 和图标标题', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');

    for (const id of [
      'set-branding',
      'set-visitor',
      'set-dist',
      'set-agent-logs',
      'set-cert',
      'set-xray',
      'set-conn',
      'set-wg',
      'set-ports',
      'set-probe',
      'set-ping-probe',
      'set-geodata',
      'set-vpngate-intelligence',
      'set-tunnel-probes',
    ]) {
      const panel = document.getElementById(id)!;
      expect(panel.classList.contains('config-panel')).toBe(true);
      expect(panel.querySelector(':scope > header .panel-title')).toBeTruthy();
      expect(panel.querySelector(':scope > header .panel-title-icon')).toBeTruthy();
      expect(panel.querySelector(':scope > header .no')).toBeNull();
    }

    for (const id of ['set-ping-probe', 'set-vpngate-intelligence', 'set-tunnel-probes']) {
      const panel = document.getElementById(id)!;
      expect(panel.querySelector(':scope > .cardsub')).toBeTruthy();
    }
    expect(document.querySelector('#set-ping-probe > .settings-block')).toBeTruthy();
    expect(
      document.querySelectorAll('#set-vpngate-intelligence > .vpngate-intelligence-layout > .settings-block'),
    ).toHaveLength(3);
    expect(document.querySelector('#set-tunnel-probes > .settings-block')).toBeTruthy();
  });

  it('端口基线明确区分 VLESS 与 AnyTLS', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');

    const ports = section('set-ports');
    expect(await ports.findByText('VLESS · TLS / REALITY')).toBeTruthy();
    expect((ports.getByDisplayValue('13443') as HTMLInputElement).value).toBe('13443');
    expect((ports.getByDisplayValue('14443') as HTMLInputElement).value).toBe('14443');
    expect(ports.queryByText('接入面')).toBeNull();
    expect(ports.getByText(/仅影响新建/)).toBeTruthy();
    expect(ports.queryByText('需要发布')).toBeNull();
    expect(ports.getByDisplayValue('13443').closest('.port-allocation-grid')).toBeTruthy();
    expect(document.getElementById('set-ports')?.querySelector('.settings-parameter-group')).toBeNull();
    expect(document.getElementById('set-ports')?.querySelector('.settings-field-grid-five')).toBeNull();
  });

  it('全局 MTU 使用明确名称，缺省值为 1280，探测区不再主动渲染风险提示', async () => {
    ROUTES['/settings'] = () => ({ ...committedSettings(), overlay: undefined });
    ROUTES['/links/mtu'] = () => ({
      default_mtu: 1280,
      nodes: [
        {
          node_id: 'node-1',
          current_mtu: 1400,
          overridden: true,
          suggested_mtu: 1399,
          tightest_peer: 'node-2',
          inconclusive: 0,
        },
      ],
      links: [
        {
          node_id: 'node-1',
          peer_node_id: 'node-2',
          endpoint_host: '192.0.2.2',
          status: 'ok',
          path_mtu: 1459,
          suggested_wg_mtu: 1399,
          probed_at: '2026-09-08T00:00:00Z',
        },
      ],
    });
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');

    const wireguard = section('set-wg');
    expect(fieldInput(wireguard, '全局 MTU').value).toBe('1280');
    expect(await wireguard.findByRole('button', { name: '看探测结果' })).toBeTruthy();
    expect(wireguard.queryByText(/大包会被打掉|生效值大过探测建议|还有余量/)).toBeNull();
  });

  it('Ping 调度与目标共用单层面板和紧凑表头', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');

    const ping = section('set-ping-probe');
    const schedule = ping.getByLabelText('Ping 探测调度');
    const targets = ping.getByText('探测目标', { selector: '.eyebrow' }).closest('.ping-probe-target-section');
    expect(schedule.classList.contains('settings-block')).toBe(false);
    expect(schedule.querySelector('.ping-probe-schedule-grid')).toBeTruthy();
    expect(targets?.classList.contains('settings-block')).toBe(false);
    expect(ping.getByRole('button', { name: '＋ TCP' })).toBeTruthy();
    expect(ping.getByRole('button', { name: '＋ ICMP' })).toBeTruthy();
    const panel = document.getElementById('set-ping-probe')!;
    expect(panel.querySelector(':scope > .cardsub')?.textContent).toContain('周期探测');
    expect(schedule.closest('.ping-probe-settings-block')).toBe(panel.querySelector(':scope > .settings-block'));
    expect(panel.querySelector('.ping-probe-timing')).toBeTruthy();
  });

  it('VLESS Encryption 起始端口默认 13800，允许保存自定义起点', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');
    const ports = section('set-ports');
    const input = ports.getByRole('spinbutton', { name: 'VLESS · Encryption 起始端口' });
    expect((input as HTMLInputElement).value).toBe('13800');
    fireEvent.change(input, { target: { value: '49000' } });
    fireEvent.click(ports.getByText('保存这一段'));
    await waitFor(() =>
      expect(immediateSettingsWrites).toContainEqual({
        path: '/settings/ports',
        body: expect.objectContaining({ vless_encryption_base: 49000 }),
      }),
    );
    expect(settingsOp()).toBeUndefined();
  });

  it('端到端探测设置即时写库，不进入草稿', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');
    const probe = section('set-probe');
    const interval = fieldInput(probe, '多久探一轮');
    fireEvent.change(interval, { target: { value: '90' } });
    fireEvent.click(probe.getByText('保存这一段'));

    await waitFor(() =>
      expect(immediateSettingsWrites).toContainEqual({
        path: '/settings/probe',
        body: expect.objectContaining({ interval_secs: 90 }),
      }),
    );
    expect(settingsOp()).toBeUndefined();
  });

  it('保存动作默认隐藏，有改动后才出现在卡片底部', async () => {
    render(<Harness />);
    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    const xray = document.getElementById('set-xray')!;

    expect(section('set-xray').queryByRole('button', { name: '保存这一段' })).toBeNull();
    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });

    expect(section('set-xray').getByRole('button', { name: '保存这一段' })).toBeTruthy();
    expect(xray.lastElementChild?.classList.contains('settings-savebar')).toBe(true);
  });

  it('中继 Mux 默认收起，收起不丢输入，保存整组参数', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');

    const connection = section('set-conn');
    expect(connection.queryByText('复用流数量')).toBeNull();
    expect(connection.getByLabelText('中继 Mux 当前参数').textContent).toContain('复用流1预热目标0复用阈值2');

    const muxToggle = connection.getByRole('button', { name: '配置中继 Mux 参数' });
    expect(muxToggle.getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(muxToggle);
    expect(muxToggle.getAttribute('aria-expanded')).toBe('true');
    const concurrency = fieldInput(connection, '复用流数量');
    const probeInterval = fieldInput(connection, '探活周期');
    const idleTtl = fieldInput(connection, '超额空闲寿命');
    expect(probeInterval.value).toBe('5000');
    expect(idleTtl.value).toBe('24000');
    expect(connection.getAllByText('ms')).toHaveLength(3);
    expect(connection.getByText('连接池', { selector: '.eyebrow' })).toBeTruthy();
    expect(connection.getByText('探活', { selector: '.eyebrow' })).toBeTruthy();
    expect(concurrency.closest('.settings-parameter-group')).toBeTruthy();
    expect(concurrency.closest('.settings-parameter-grid')).toBeTruthy();
    fireEvent.change(concurrency, { target: { value: '8' } });
    fireEvent.change(probeInterval, { target: { value: '6125' } });
    fireEvent.change(idleTtl, { target: { value: '25125' } });
    expect(connection.getByLabelText('中继 Mux 当前参数').textContent).toContain('复用流8预热目标0复用阈值2');

    fireEvent.click(connection.getByRole('button', { name: '收起中继 Mux 参数' }));
    expect(connection.queryByText('复用流数量')).toBeNull();
    fireEvent.click(connection.getByRole('button', { name: '配置中继 Mux 参数' }));
    expect(fieldInput(connection, '复用流数量').value).toBe('8');
    expect(fieldInput(connection, '探活周期').value).toBe('6125');
    expect(fieldInput(connection, '超额空闲寿命').value).toBe('25125');

    fireEvent.click(connection.getByRole('button', { name: '保存这一段' }));
    await waitFor(() =>
      expect(settingsOp()).toMatchObject({
        settings: {
          relay_mux: {
            concurrency: 8,
            prewarm_workers: 0,
            reuse_threshold: 2,
            probe_interval_ms: 6125,
            idle_ttl_ms: 25125,
          },
        },
      }),
    );
  });

  it('中继 Mux 的交叉约束会禁用保存，只读角色仍可展开查看', async () => {
    const editableView = render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');
    let connection = section('set-conn');
    fireEvent.click(connection.getByRole('button', { name: '配置中继 Mux 参数' }));
    const minIdle = fieldInput(connection, '预热目标');
    const maxIdle = fieldInput(connection, '复用阈值');
    fireEvent.change(maxIdle, { target: { value: '1' } });
    fireEvent.change(minIdle, { target: { value: '2' } });
    expect(connection.getByText('预热目标不能大于复用阈值')).toBeTruthy();
    expect((connection.getByRole('button', { name: '保存这一段' }) as HTMLButtonElement).disabled).toBe(true);

    editableView.unmount();
    render(<Harness role="editor" />);
    await screen.findByPlaceholderText('example.com:443');
    connection = section('set-conn');
    fireEvent.click(connection.getByRole('button', { name: '查看中继 Mux 参数' }));
    expect(connection.getByText('复用流数量')).toBeTruthy();
    // 外层 fieldset 统一控制只读态，后代 input 不会自动获得 disabled
    // attribute，但在浏览器的有效禁用状态中会匹配 :disabled。
    expect(fieldInput(connection, '复用流数量').matches(':disabled')).toBe(true);
  });

  it('立即签发只入队后台任务并显示持久化进度，签发配置与证书记录分开', async () => {
    let full = certsWithGroup();
    const queued = {
      id: 41,
      trigger: 'manual' as const,
      status: 'queued' as const,
      phase: 'queued' as const,
      total_items: 0,
      processed_items: 0,
      issued_items: 0,
      failed_items: 0,
      current_certificate_id: null,
      current_subject: null,
      error_detail: null,
      queued_at_unix_secs: 1,
      started_at_unix_secs: null,
      heartbeat_at_unix_secs: null,
      finished_at_unix_secs: null,
    };
    const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/certs/scan' && init?.method === 'POST') {
        full = { ...full, scan: queued };
        return Response.json(queued, { status: 202 });
      }
      const body = path === '/certs' ? () => full : ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return Response.json(body());
    });
    vi.stubGlobal('fetch', fetchMock);
    render(<Harness />);
    const process = await screen.findByRole('button', { name: '立即签发与续期' });
    expect(screen.getByText('签发配置')).toBeTruthy();
    const selfSigned = screen.getByText('自签证书', { selector: '.cert-method-config-name' }).closest('details');
    const publicCa = screen.getByText("Let's Encrypt", { selector: '.cert-method-config-name' }).closest('details');
    expect(selfSigned?.open).toBe(false);
    expect(publicCa?.open).toBe(false);
    expect(screen.getByText('已配置 · 固定 A/B 主备')).toBeTruthy();
    expect(screen.queryByText(/不代表当前选中了哪一种/)).toBeNull();
    expect(screen.queryByText('现在检查一轮')).toBeNull();
    fireEvent.click(process);
    expect(await screen.findByText('后台签发任务')).toBeTruthy();
    expect(screen.getByText('#41 · 手动触发')).toBeTruthy();
    expect(screen.getByText('等待后台处理')).toBeTruthy();
    expect((screen.getByRole('button', { name: '后台处理中…' }) as HTMLButtonElement).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledWith('/certs/scan', expect.objectContaining({ method: 'POST' }));
  });

  it('两项签发配置同时展示，只在新建证书组时选择类型', async () => {
    const full = certsWithGroup();
    full.groups[0].name = '默认自签证书组';
    full.groups[0].is_default = true;
    ROUTES['/certs'] = () => full;
    render(<Harness />);

    await screen.findByText('默认自签证书组');
    const publicCa = screen.getByText("Let's Encrypt", { selector: '.cert-method-config-name' }).closest('details')!;
    expect(publicCa.open).toBe(false);
    fireEvent.click(publicCa.querySelector('summary')!);
    expect(publicCa.open).toBe(true);
    expect(within(publicCa).getByPlaceholderText('example.net')).toBeTruthy();
    const token = within(publicCa).getByPlaceholderText('已配置（重填才会覆盖）') as HTMLInputElement;
    expect(token.type).toBe('text');
    expect(token.autocomplete).toBe('off');
    expect(token.classList.contains('config-secret-input')).toBe(true);
    expect(screen.queryByRole('group', { name: '证书组类型' })).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '新建证书组' }));
    const method = screen.getByRole('group', { name: '证书组类型' });
    const selfSigned = within(method).getByRole('button', { name: '自签证书' });
    const letsEncrypt = within(method).getByRole('button', { name: "Let's Encrypt + Cloudflare DNS" });
    expect(selfSigned.getAttribute('aria-pressed')).toBe('false');
    expect(letsEncrypt.getAttribute('aria-pressed')).toBe('false');
    fireEvent.click(selfSigned);
    expect(selfSigned.getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(letsEncrypt);
    expect(letsEncrypt.getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(selfSigned);
    expect(screen.getByPlaceholderText('example.net')).toBeTruthy();
    expect(screen.getByText(/创建后立即生成并签发固定主备两份/)).toBeTruthy();
  });

  it('把同一证书的通配名与裸名合并成紧凑名称', async () => {
    const full = certsWithGroup();
    ROUTES['/certs'] = () => full;
    render(<Harness />);

    fireEvent.click(await screen.findByRole('button', { name: /CA-1/ }));

    expect(screen.getByText('[*.]a1b2c3d4.private.example')).toBeTruthy();
    expect(screen.queryByText('*.a1b2c3d4.private.example · a1b2c3d4.private.example')).toBeNull();
  });

  it('证书组默认收起，自签模式不提供手动添加备用动作', async () => {
    const full = certsWithGroup();
    full.groups[0].name = '默认自签证书组';
    full.groups[0].is_default = true;
    // 即使旧数据里的组配置字段损坏，也必须以当前 serving 证书的冻结签发方式为准，
    // 不能再把实际自签的组标成 Let's Encrypt。
    full.groups[0].signing_method = 'public-ca';
    full.groups[0].names = ['northstar-edge-0123abcd.com'];
    full.groups[0].certificates = Array.from({ length: 2 }, (_, index) => ({
      id: `cert-${index}`,
      status: index === 0 ? 'serving' : 'compatible',
      origin: 'bootstrap',
      signing_method: 'self-signed',
      certificate_name: `private-${index}.com`,
      runtime_slot: index === 0 ? 'a' : 'b',
      issuer: 'Northstar Edge Root CA',
      issued_at: '2026-01-01T00:00:00Z',
      expires_at: '2126-01-01T00:00:00Z',
      sha256: `${index}`.padStart(64, '0'),
      attempts: 0,
      last_error: null,
      last_attempt_at: null,
    }));
    full.nodes = [
      {
        node_id: 'hidden-node',
        label_id: 'group-1',
        group_name: '默认自签证书组',
        certificate_name: 'private-0.com',
        on_disk: 'current',
        observed_at: '2026-01-01T00:01:00Z',
      },
    ];
    ROUTES['/certs'] = () => full;
    render(<Harness />);

    const toggle = await screen.findByRole('button', { name: /默认自签证书组/ });
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(screen.getAllByText('自签证书').length).toBeGreaterThan(0);
    expect(screen.getByText('组配置类型异常')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /增加证书/ })).toBeNull();
    fireEvent.click(toggle.closest('header')!);
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    fireEvent.click(toggle.closest('header')!);
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-expanded')).toBe('true');

    expect(screen.getByLabelText('证书组完整信息').textContent).toContain('运行槽2/2');
    expect(screen.getByText('在用').classList.contains('ok')).toBe(true);
    expect(screen.getByText('保留').classList.contains('idle')).toBe(true);
    expect(screen.getByRole('button', { name: '切换' })).toBeTruthy();
    expect(screen.getByText('private-0.com')).toBeTruthy();
    expect(screen.getByText('cert-0')).toBeTruthy();
    expect(screen.getAllByText('Northstar Edge Root CA')).toHaveLength(2);
    expect(screen.queryByText('hidden-node')).toBeNull();
    expect(screen.queryByText('使用机器')).toBeNull();
    expect((screen.getByRole('button', { name: '改名' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('添加备用证书期间锁住证书动作，连续点击只提交一次', async () => {
    const publicCa = publicCaCertsWithGroup();
    publicCa.groups[0].certificates = [
      {
        id: 'ready-existing',
        status: 'ready',
        origin: 'spare',
        signing_method: 'public-ca',
        certificate_name: null,
        runtime_slot: null,
        issuer: "Let's Encrypt",
        issued_at: '2026-01-01T00:00:00Z',
        expires_at: '2026-12-01T00:00:00Z',
        sha256: 'a'.repeat(64),
        attempts: 0,
        last_error: null,
        last_attempt_at: null,
      },
    ];
    let resolveSpare!: (response: Response) => void;
    const spareResponse = new Promise<Response>(resolve => {
      resolveSpare = resolve;
    });
    const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/certs/groups/group-1/spare' && init?.method === 'POST') return spareResponse;
      const body = path === '/certs' ? () => publicCa : ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return new Response(JSON.stringify(body()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<Harness />);
    const add = await screen.findByRole('button', { name: /增加证书/ });
    expect((add as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(add);
    fireEvent.click(add);

    await waitFor(() => {
      const posts = fetchMock.mock.calls.filter(
        ([path, init]) => path === '/certs/groups/group-1/spare' && init?.method === 'POST',
      );
      expect(posts).toHaveLength(1);
    });
    expect((screen.getByRole('button', { name: '正在申领…' }) as HTMLButtonElement).disabled).toBe(true);

    resolveSpare(
      new Response(JSON.stringify({ id: 'spare-1' }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
    await waitFor(() =>
      expect((screen.getByRole('button', { name: /增加证书/ }) as HTMLButtonElement).disabled).toBe(false),
    );
  });

  it('证书组创建失败时保留表单和输入，不把失败表现成已完成', async () => {
    const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/certs/groups' && init?.method === 'POST') {
        return new Response(JSON.stringify({ error: '证书组写入失败' }), {
          status: 500,
          headers: { 'content-type': 'application/json' },
        });
      }
      const body = path === '/certs' ? certsWithGroup : ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return new Response(JSON.stringify(body()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<Harness />);
    fireEvent.click(await screen.findByRole('button', { name: '新建证书组' }));
    fireEvent.click(
      within(screen.getByRole('group', { name: '证书组类型' })).getByRole('button', { name: '自签证书' }),
    );
    const name = screen.getByPlaceholderText('香港前置') as HTMLInputElement;
    fireEvent.change(name, { target: { value: '新加坡备用' } });
    fireEvent.click(screen.getByRole('button', { name: '创建并立即申领' }));

    await screen.findByText(/证书组写入失败/);
    const createCall = fetchMock.mock.calls.find(([path, init]) => path === '/certs/groups' && init?.method === 'POST');
    expect(JSON.parse(String(createCall?.[1]?.body))).toMatchObject({
      name: '新加坡备用',
      signing_method: 'self-signed',
    });
    expect(screen.getByPlaceholderText('香港前置')).toBe(name);
    expect(name.value).toBe('新加坡备用');
  });

  it('非系统管理员看到的设置控件是真只读，不会产生无法保存的脏表单', async () => {
    render(<Harness role="editor" />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    expect(dest.matches(':disabled')).toBe(true);
    expect(section('set-xray').queryByRole('button', { name: '保存这一段' })).toBeNull();
  });

  it('保存一段之后该段不再显示「有未保存的改动」', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    expect(dest.value).toBe('www.committed.example:443');

    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    expect(section('set-xray').getByText('有未保存的改动')).toBeTruthy();

    fireEvent.click(section('set-xray').getByText('保存这一段'));

    /* 改动确实进了草稿 */
    await waitFor(() =>
      expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } }),
    );

    /* 但基准仍是 GET /settings 的已提交值，该段会一直自认为有未保存的改动 */
    await waitFor(() => expect(section('set-xray').queryByText('有未保存的改动')).toBeNull());
  });

  it('保存站点名称后，较晚返回的旧请求不会覆盖新名称', async () => {
    const old = { site_name: '旧站点', icon_data_url: null };
    const saved = { site_name: '新站点', icon_data_url: null };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    client.setQueryData(['branding'], old);
    let complete!: (response: Response) => void;
    const staleResponse = new Promise<Response>(resolve => {
      complete = resolve;
    });
    const fetcher = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/branding') return init?.method === 'PUT' ? Response.json(saved) : staleResponse;
      const body = ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return Response.json(body());
    });
    vi.stubGlobal('fetch', fetcher);
    render(
      <QueryClientProvider client={client}>
        <SessionProvider value={SESSION}>
          <SettingsPane />
        </SessionProvider>
      </QueryClientProvider>,
    );
    await screen.findByPlaceholderText('example.com:443');
    const name = fieldInput(section('set-branding'), '站点名称');
    fireEvent.change(name, { target: { value: '新站点' } });
    fireEvent.click(section('set-branding').getByText('保存这一段'));
    await waitFor(() => expect(client.getQueryData(['branding'])).toEqual(saved));
    await act(async () => {
      complete(Response.json(old));
      await staleResponse;
    });
    expect(client.getQueryData(['branding'])).toEqual(saved);
    expect(name.value).toBe('新站点');
  });

  it('分发版本刷新不覆盖正在输入的 Agent 地址', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <SessionProvider value={SESSION}>
          <SettingsPane />
        </SessionProvider>
      </QueryClientProvider>,
    );
    await screen.findByPlaceholderText('example.com:443');
    const address = fieldInput(section('set-dist'), 'Agent 请求地址');
    fireEvent.change(address, { target: { value: 'https://new-control.example' } });
    act(() =>
      client.setQueryData(['distribution'], {
        stored: { agent_public_url: 'https://example', xray_version: '26.7.28' },
        effective: { agent_public_url: 'https://example', xray_version: 'new-build' },
      }),
    );
    expect(address.value).toBe('https://new-control.example');
    expect(section('set-dist').getByText('有未保存的改动')).toBeTruthy();
  });

  it('丢弃草稿恢复已保存字段，并保留其他段尚未保存的输入', async () => {
    render(<Harness />);
    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    const hopBase = (await screen.findByLabelText('中转端口起始值')) as HTMLInputElement;
    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    fireEvent.click(section('set-xray').getByText('保存这一段'));
    await waitFor(() => expect(section('set-xray').queryByText('有未保存的改动')).toBeNull());
    fireEvent.change(hopBase, { target: { value: '20100' } });
    act(() => draft.clear());
    await waitFor(() => expect(dest.value).toBe('www.committed.example:443'));
    expect(section('set-xray').queryByText('有未保存的改动')).toBeNull();
    expect(hopBase.value).toBe('20100');
    expect(section('set-ports').getByText('有未保存的改动')).toBeTruthy();
  });

  it('保存后使用规范化值，不因空格或前导零再次显示保存按钮', async () => {
    render(<Harness />);
    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    fireEvent.change(dest, { target: { value: '  www.edited.example:443  ' } });
    fireEvent.click(section('set-xray').getByText('保存这一段'));
    await waitFor(() => expect(dest.value).toBe('www.edited.example:443'));
    expect(section('set-xray').queryByText('有未保存的改动')).toBeNull();
    const hopBase = (await screen.findByLabelText('中转端口起始值')) as HTMLInputElement;
    fireEvent.change(hopBase, { target: { value: '020100' } });
    fireEvent.click(section('set-ports').getByText('保存这一段'));
    await waitFor(() => expect(hopBase.value).toBe('20100'));
    expect(section('set-ports').queryByText('有未保存的改动')).toBeNull();
  });

  it('即时保存端口不会覆盖机器设置草稿，草稿也不会覆盖新的端口基准', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    fireEvent.click(section('set-xray').getByText('保存这一段'));
    await waitFor(() =>
      expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } }),
    );

    /* 再改另一段并保存。两段互不相干，前一段已经在草稿里，不应被这次保存覆盖回去。 */
    const hopBase = (await screen.findByLabelText('中转端口起始值')) as HTMLInputElement;
    fireEvent.change(hopBase, { target: { value: '20100' } });
    fireEvent.click(section('set-ports').getByText('保存这一段'));

    await waitFor(() =>
      expect(immediateSettingsWrites).toContainEqual({
        path: '/settings/ports',
        body: expect.objectContaining({ hop_base: 20100 }),
      }),
    );
    expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } });
    expect(hopBase.value).toBe('20100');
    expect(section('set-ports').queryByText('有未保存的改动')).toBeNull();
  });

  /* 分段保存的本意：保存 A 段时，B 段「改了但没点保存」的输入既不能被提交，也不能被抹掉。
     基准改成草稿之后这条仍须成立——重填表单的触发条件因此没有跟着草稿走。 */
  it('保存一段不会提交、也不会抹掉另一段未保存的输入', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    const hopBase = (await screen.findByLabelText('中转端口起始值')) as HTMLInputElement;

    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    fireEvent.change(hopBase, { target: { value: '20100' } });

    fireEvent.click(section('set-xray').getByText('保存这一段'));

    await waitFor(() =>
      expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } }),
    );
    /* 端口段没点保存，不进草稿 */
    expect(settingsOp()).toMatchObject({ settings: { ports: { hop_base: 20000 } } });
    /* 但输入框里的值要留着，并且该段仍标为有未保存的改动 */
    expect(hopBase.value).toBe('20100');
    expect(section('set-ports').getByText('有未保存的改动')).toBeTruthy();
  });
});

it('反向隧道参数位于连接策略，统一保存并保留到其他段的草稿', async () => {
  render(<Harness />);
  await screen.findByPlaceholderText('example.com:443');
  const connection = section('set-conn');
  expect(connection.getByLabelText('反向隧道恢复设置')).toBeTruthy();
  /* 面板默认收起；展开后按语义分组，不再套第二层「高级参数」。 */
  const expand = connection.getByRole('button', { name: '配置反向隧道参数' });
  expect(expand.getAttribute('aria-expanded')).toBe('false');
  expect(connection.queryByText('业务探测')).toBeNull();
  fireEvent.click(expand);
  expect(expand.getAttribute('aria-expanded')).toBe('true');
  expect(connection.queryByText('业务探测')).toBeNull();
  expect(connection.getByText('故障处理')).toBeTruthy();
  expect(connection.getByRole('button', { name: '保留' }).getAttribute('aria-pressed')).toBe('true');
  expect(connection.queryByRole('button', { name: '添加定向覆盖' })).toBeNull();
  fireEvent.change(connection.getByLabelText('恢复所需连续应答次数'), { target: { value: '3' } });
  fireEvent.click(connection.getByRole('button', { name: '主动断开' }));
  fireEvent.click(connection.getByRole('button', { name: '保存这一段' }));
  await waitFor(() =>
    expect(settingsOp()).toMatchObject({
      settings: {
        reverse_health: { disconnect_on_health_failure: true, tuning: { recovery_successes: 3 } },
        reverse_health_overrides: [],
      },
    }),
  );
  await waitFor(() => expect(connection.queryByRole('button', { name: '保存这一段' })).toBeNull());
  fireEvent.change(screen.getByPlaceholderText('example.com:443'), { target: { value: 'new.example:443' } });
  fireEvent.click(section('set-xray').getByRole('button', { name: '保存这一段' }));
  await waitFor(() =>
    expect(settingsOp()).toMatchObject({
      settings: {
        reality_site: { dest: 'new.example:443' },
        reverse_health: { tuning: { recovery_successes: 3 } },
        reverse_health_overrides: [],
      },
    }),
  );
});

it('反向隧道参数无效时禁止保存连接策略', async () => {
  render(<Harness />);
  await screen.findByPlaceholderText('example.com:443');
  const connection = section('set-conn');
  fireEvent.click(connection.getByRole('button', { name: '配置反向隧道参数' }));
  expect(connection.getByLabelText('首次超时（毫秒）')).toBeTruthy();
  fireEvent.change(connection.getByLabelText('首次超时（毫秒）'), { target: { value: '1000' } });
  const save = connection.getByRole('button', { name: '保存这一段' }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  expect(connection.getByRole('alert').textContent).toContain('参数超出范围');
  fireEvent.click(save);
  expect(settingsOp()).toBeUndefined();
});
