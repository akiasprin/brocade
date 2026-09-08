/* 设置页的分段保存写的是草稿（`saveSettings` → `draft.push({op:'update_settings'})`），
 * 而基准 `pristine` 读的是 `GET /settings`——直连接口，草稿提交前不会变。
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
import type { CertsView } from '../src/api';

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
  who: {
    operator_id: 'tester',
    role: 'system-admin' as const,
    tenant_scope: null,
    token_prefix: null,
    masked_assets: false,
  },
};

/** 已提交的全局设置。分段保存写草稿，这一份始终不变——正是问题所在。 */
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
    min_idle_workers: 0,
    max_idle_workers: 2,
    max_probing_workers: 1,
    probe_interval_secs: 5,
    probe_timeout_ms: 2000,
    idle_ttl_secs: 24,
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
  '/ping-probe/settings': () => ({ targets: [], interval_secs: 60, timeout_ms: 420 }),
  '/links/mtu': () => ({ default_mtu: 1420, nodes: [], links: [] }),
  '/revisions?limit=50': () => ({ current_revision: 7, revisions: [{ id: 7 }] }),
};

const certsWithGroup = (): CertsView => ({
  sealing_available: true,
  domain: {
    id: 'private.example',
    domain: 'private.example',
    dns_provider: 'cloudflare',
    acme_directory: 'self-signed',
    signing_method: 'self-signed',
    acme_contact: null,
    renew_before_days: 30,
    has_credential: false,
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
    vi.fn(async (path: string) => {
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
      <SessionProvider value={{ who: { ...SESSION.who, role } }}>
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
  const input = scope.getByText(label).closest('.setfld')?.querySelector('input');
  if (!(input instanceof HTMLInputElement)) throw new Error(`没有找到字段 ${label}`);
  return input;
};

const settingsOp = () => draft.ops().find(op => op.op === 'update_settings');

beforeEach(() => {
  draft.init(`settings-baseline-${Math.random()}`);
  draft.clear();
  stubFetch();
});

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
  ROUTES['/certs'] = () => ({
    groups: [],
    nodes: [],
    letsencrypt: 'https://acme',
    letsencrypt_staging: 'https://acme-staging',
  });
});

describe('设置页分段保存的基准', () => {
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
    ]) {
      const panel = document.getElementById(id)!;
      expect(panel.classList.contains('config-panel')).toBe(true);
      expect(panel.querySelector(':scope > header .panel-title')).toBeTruthy();
      expect(panel.querySelector(':scope > header .panel-title-icon')).toBeTruthy();
      expect(panel.querySelector(':scope > header .no')).toBeNull();
    }
  });

  it('端口基线明确区分 VLESS 与 AnyTLS', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');

    const ports = section('set-ports');
    expect(await ports.findByText('VLESS · TLS / REALITY')).toBeTruthy();
    expect((ports.getByDisplayValue('13443') as HTMLInputElement).value).toBe('13443');
    expect((ports.getByDisplayValue('14443') as HTMLInputElement).value).toBe('14443');
    expect(ports.queryByText('接入面')).toBeNull();
    expect(ports.getByText('仅影响新建')).toBeTruthy();
    expect(ports.queryByText('需要发布')).toBeNull();
  });

  it('VLESS Encryption 起始端口默认 48000，允许保存自定义起点', async () => {
    render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');
    const ports = section('set-ports');
    const input = ports.getByRole('spinbutton', { name: 'VLESS · Encryption 起始端口' });
    expect((input as HTMLInputElement).value).toBe('48000');
    fireEvent.change(input, { target: { value: '49000' } });
    fireEvent.click(ports.getByText('保存这一段'));
    await waitFor(() => expect(settingsOp()).toMatchObject({ settings: { ports: { vless_encryption_base: 49000 } } }));
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
    expect(connection.getByText(/\u590d用流 1 · 空闲 0–2/)).toBeTruthy();

    fireEvent.click(connection.getByRole('button', { name: '配置' }));
    const concurrency = fieldInput(connection, '复用流数量');
    fireEvent.change(concurrency, { target: { value: '8' } });
    expect(connection.getByText(/\u590d用流 8 · 空闲 0–2/)).toBeTruthy();

    fireEvent.click(connection.getByRole('button', { name: '收起' }));
    expect(connection.queryByText('复用流数量')).toBeNull();
    fireEvent.click(connection.getByRole('button', { name: '配置' }));
    expect(fieldInput(connection, '复用流数量').value).toBe('8');

    fireEvent.click(connection.getByRole('button', { name: '保存这一段' }));
    await waitFor(() =>
      expect(settingsOp()).toMatchObject({
        settings: { relay_mux: { concurrency: 8, min_idle_workers: 0, max_idle_workers: 2 } },
      }),
    );
  });

  it('中继 Mux 的交叉约束会禁用保存，只读角色仍可展开查看', async () => {
    const editableView = render(<Harness />);
    await screen.findByPlaceholderText('example.com:443');
    let connection = section('set-conn');
    fireEvent.click(connection.getByRole('button', { name: '配置' }));
    const idleInputs = connection.getByText('空闲连接').closest('.setfld')?.querySelectorAll('input');
    if (!idleInputs || idleInputs.length !== 2) throw new Error('没有找到空闲连接上下限');
    fireEvent.change(idleInputs[1], { target: { value: '1' } });
    fireEvent.change(idleInputs[0], { target: { value: '2' } });
    expect(connection.getByText('最小空闲连接不能大于最大空闲连接')).toBeTruthy();
    expect((connection.getByRole('button', { name: '保存这一段' }) as HTMLButtonElement).disabled).toBe(true);

    editableView.unmount();
    render(<Harness role="editor" />);
    await screen.findByPlaceholderText('example.com:443');
    connection = section('set-conn');
    fireEvent.click(connection.getByRole('button', { name: '查看' }));
    expect(connection.getByText('复用流数量')).toBeTruthy();
    // 外层 fieldset 统一控制只读态，后代 input 不会自动获得 disabled
    // attribute，但在浏览器的有效禁用状态中会匹配 :disabled。
    expect(fieldInput(connection, '复用流数量').matches(':disabled')).toBe(true);
  });

  it('立即签发等待完成并显示结果，申领设置与证书记录分开', async () => {
    const full = certsWithGroup();
    let complete!: (response: Response) => void;
    const response = new Promise<Response>(resolve => {
      complete = resolve;
    });
    const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/certs/scan' && init?.method === 'POST') return response;
      const body = path === '/certs' ? () => full : ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return Response.json(body());
    });
    vi.stubGlobal('fetch', fetchMock);
    render(<Harness />);
    const process = await screen.findByRole('button', { name: '立即签发与续期' });
    const settingsTitle = screen.getByText('新证书申领设置');
    expect(settingsTitle.closest('details')?.open).toBe(false);
    expect(screen.getByText(/修改设置不会改写已签发的证书/)).toBeTruthy();
    expect(screen.queryByText('现在检查一轮')).toBeNull();
    fireEvent.click(process);
    await waitFor(() =>
      expect((screen.getByRole('button', { name: '正在签发与续期…' }) as HTMLButtonElement).disabled).toBe(true),
    );
    expect(screen.queryByText(/处理完成/)).toBeNull();
    complete(Response.json({ ...full, processing: { issued: 2, failed: 1 } }));
    expect(await screen.findByText(/处理完成：成功 2 张，失败 1 张/)).toBeTruthy();
    expect((screen.getByRole('button', { name: '立即签发与续期' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('自签模式隐藏全局域名输入并说明百年随机身份', async () => {
    ROUTES['/certs'] = certsWithGroup;
    render(<Harness />);

    await screen.findByText(/默认自签证书组首次初始化一对主备证书/);
    expect(screen.queryByPlaceholderText('example.net')).toBeNull();
    expect(screen.getByText(/单张有效期 100 年/)).toBeTruthy();
    expect(screen.getByText(/不再拼接二级域名或通配符/)).toBeTruthy();
  });

  it('证书组默认收起，自签模式不提供手动添加备用动作', async () => {
    const full = certsWithGroup();
    full.groups[0].name = '默认自签证书组';
    full.groups[0].is_default = true;
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
    expect(screen.queryByRole('button', { name: '立即申领备用证书' })).toBeNull();
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
    const add = await screen.findByRole('button', { name: '立即申领备用证书' });
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
      expect((screen.getByRole('button', { name: '立即申领备用证书' }) as HTMLButtonElement).disabled).toBe(false),
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
    const name = screen.getByPlaceholderText('香港前置') as HTMLInputElement;
    fireEvent.change(name, { target: { value: '新加坡备用' } });
    fireEvent.click(screen.getByRole('button', { name: '创建并立即申领' }));

    await screen.findByText(/证书组写入失败/);
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
    const hopBase = (await screen.findByDisplayValue('20000')) as HTMLInputElement;
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
    const hopBase = (await screen.findByDisplayValue('20000')) as HTMLInputElement;
    fireEvent.change(hopBase, { target: { value: '020100' } });
    fireEvent.click(section('set-ports').getByText('保存这一段'));
    await waitFor(() => expect(hopBase.value).toBe('20100'));
    expect(section('set-ports').queryByText('有未保存的改动')).toBeNull();
  });

  it('保存另一段不会把前一段已入草稿的改动写回旧值', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    fireEvent.click(section('set-xray').getByText('保存这一段'));
    await waitFor(() =>
      expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } }),
    );

    /* 再改另一段并保存。两段互不相干，前一段已经在草稿里，不应被这次保存覆盖回去。 */
    const hopBase = (await screen.findByDisplayValue('20000')) as HTMLInputElement;
    fireEvent.change(hopBase, { target: { value: '20100' } });
    fireEvent.click(section('set-ports').getByText('保存这一段'));

    await waitFor(() => expect(settingsOp()).toMatchObject({ settings: { ports: { hop_base: 20100 } } }));
    expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } });
  });

  /* 分段保存的本意：保存 A 段时，B 段「改了但没点保存」的输入既不能被提交，也不能被抹掉。
     基准改成草稿之后这条仍须成立——重填表单的触发条件因此没有跟着草稿走。 */
  it('保存一段不会提交、也不会抹掉另一段未保存的输入', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    const hopBase = (await screen.findByDisplayValue('20000')) as HTMLInputElement;

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
  const advanced = connection.getByText('高级参数：探活、容量、退避与业务探测');
  fireEvent.click(advanced);
  fireEvent.change(connection.getByLabelText('恢复所需连续应答次数'), { target: { value: '3' } });
  fireEvent.change(connection.getByLabelText('业务探测间隔（毫秒）'), { target: { value: '2000' } });
  fireEvent.click(connection.getByRole('button', { name: '添加定向覆盖' }));
  const row = within(connection.getByText('定向链路覆盖 1').closest('fieldset')!);
  for (const [label, value] of [
    ['链路 ID', 'c1'],
    ['流量起点节点 ID', 'n1'],
    ['流量终点节点 ID', 'n2'],
  ]) {
    fireEvent.change(row.getByLabelText(label), { target: { value } });
  }
  fireEvent.change(row.getByLabelText('每条隧道业务并发上限'), { target: { value: '4' } });
  fireEvent.click(connection.getByRole('button', { name: '保存这一段' }));
  await waitFor(() =>
    expect(settingsOp()).toMatchObject({
      settings: {
        reverse_health: { tuning: { recovery_successes: 3, canary_interval_ms: 2000 } },
        reverse_health_overrides: [
          { chain: 'c1', from: 'n1', to: 'n2', health: { tuning: { max_sessions_per_worker: 4 } } },
        ],
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
        reverse_health_overrides: [{ health: { tuning: { max_sessions_per_worker: 4 } } }],
      },
    }),
  );
});

it('反向隧道高级参数无效时禁止保存连接策略', async () => {
  render(<Harness />);
  await screen.findByPlaceholderText('example.com:443');
  const connection = section('set-conn');
  fireEvent.click(connection.getByText('高级参数：探活、容量、退避与业务探测'));
  fireEvent.change(connection.getByLabelText('业务探测超时（毫秒）'), { target: { value: '2000' } });
  const save = connection.getByRole('button', { name: '保存这一段' }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  fireEvent.click(save);
  expect(settingsOp()).toBeUndefined();
});
