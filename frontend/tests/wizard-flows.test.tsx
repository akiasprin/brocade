import { changeControl } from './choice-helpers';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ChainWizard } from '../src/panes/chain-wizard';
import { draft } from '../src/draft';
import { ProvisionForm, ProvisionInstall } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { ConsoleSnapshot, ProvisionNodeResult } from '../src/api';
import * as ruleEditor from '../src/panes/rules';
import * as platform from '../src/ui/platform';

const operator = {
  operator_id: 'admin',
  role: 'system-admin' as const,
  tenant_scope: 'platform',
  token_prefix: null,
  masked_assets: false,
};
const initial = { node_count: 0, chain_group_count: [] };

const agentNodes = [
  {
    node_id: 'hk-edge-01',
    tenant_id: 'platform',
    name: '香港边缘',
    public_ipv4: '203.0.113.10',
    public_ipv4_country: 'HK',
    public_ipv6: '2001:db8::10',
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    retired_at: null,
  },
  {
    node_id: 'sg-relay-01',
    tenant_id: 'platform',
    name: '新加坡中转',
    public_ipv4: '198.51.100.20',
    public_ipv4_country: 'SG',
    public_ipv6: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    retired_at: null,
  },
];

// 改成「转发给」时目标菜单会自动打开（与链路规则一致）；已经打开就不再点，否则会把它关上。
function openTargetMenu(name: string) {
  const trigger = screen.getByRole('button', { name });
  if (trigger.getAttribute('aria-expanded') !== 'true') fireEvent.click(trigger);
  return document.body.querySelector('.external-target-menu') as HTMLElement;
}

// 路径上几台机器可以同时展开；入口机器的规则表是树里的第一张。
function entryRules() {
  return within(document.body.querySelector('.chain-rule-tree > .chain-rule-node > .chain-rule-body') as HTMLElement);
}

function queryClient() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY }, mutations: { retry: false } },
  });
  client.setQueryData(['nodes'], { nodes: agentNodes });
  client.setQueryData(['tenants'], {
    tenants: [{ id: 'platform', name: '平台', node_count: 2, user_count: 3, operator_count: 1 }],
  });
  client.setQueryData(['certs'], {
    domain: 'nodes.example.net',
    groups: [
      {
        id: 'default',
        name: '公网入口',
        is_default: true,
        names: ['*.nodes.example.net', 'hk.nodes.example.net'],
        certificates: [],
      },
    ],
    nodes: [],
  });
  client.setQueryData(['settings'], {
    ports: {
      ingress_base: 13443,
      vless_encryption_base: 13800,
      anytls_base: 14443,
      hy2_base: 30000,
      mtproto_base: 28800,
      hop_base: 20000,
    },
    reality_site: { dest: 'www.cloudflare.com:443', server_names: ['www.cloudflare.com'] },
  });
  client.setQueryData(['revisions'], { current_revision: 12, revisions: [{ id: 12 }] });
  client.setQueryData(['compile', 12], { diagnostics: [], system: { nodes: [] } });
  client.setQueryData(['users'], {
    users: [
      { tenant_id: 'platform', id: 'alice', status: 'active', created_at: '', created_revision: 1 },
      { tenant_id: 'platform', id: 'bob', status: 'active', created_at: '', created_revision: 1 },
      { tenant_id: 'platform', id: 'legacy-user', status: 'disabled', created_at: '', created_revision: 1 },
    ],
  });
  client.setQueryData(['snapshot'], {
    snapshot: {
      revision: 12,
      nodes: [
        { id: 'hk-edge-01', certificate_name: 'hk.nodes.example.net', certificate_track: 'public-ca' },
        { id: 'sg-relay-01', certificate_name: 'sg.nodes.example.net', certificate_track: 'public-ca' },
      ],
      apps: [{ id: 'global', label: '全球加速', chains: [], ingresses: [], steps: [], fronts: [], grants: [] }],
      external_outbounds: [],
    },
    node_egress_dns: [],
    redacted: false,
  });
  return client;
}

function installResult(expiresAt: string | null = null): ProvisionNodeResult {
  return {
    revision_id: 13,
    node: {
      id: 'hk-edge-01',
      tenant_id: 'platform',
      name: '香港边缘',
      public_ipv4: '203.0.113.10',
      public_ipv6: '2001:db8::10',
      public_ipv4_nat: false,
      public_ipv6_nat: false,
      overlay_addr: '10.255.0.2',
      wg_public_key: 'public-key',
      wg_listen_port: 51820,
      api_port: null,
      overlay: true,
      egress_allowed: true,
      dns: { t: 'system' },
      domain_strategy: 'use_ip',
    },
    enrollment: {
      token: 'once-secret',
      token_prefix: 'once',
      expires_at: expiresAt,
      script_url: 'https://console.example/install.sh',
      script_sha256: 'sha256',
      install_command: 'expired-install-command',
    },
  };
}

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('纳管机器向导', () => {
  it('复用详情纸面和配置卡，默认折叠 DNS/XRAY 并实时显示摘要', () => {
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <ProvisionForm go={() => undefined} />
      </QueryClientProvider>,
    );
    expect(view.container.querySelectorAll('.nd-paper')).toHaveLength(1);
    expect(view.container.querySelectorAll('.wz-head, .wz-summary, .wz-foot')).toHaveLength(0);
    expect(view.queryByRole('group', { name: '是否安装 OpenVPN 扩展' })).toBeNull();
    const dns = view.getByRole('heading', { name: 'DNS' }).closest('details')!;
    const xray = view.getByRole('heading', { name: 'XRAY' }).closest('details')!;
    expect(dns.open).toBe(false);
    expect(xray.open).toBe(false);
    expect(dns.querySelector('summary')?.textContent).toContain('system · UseIP');
    fireEvent.change(view.getByLabelText('机器 ID'), { target: { value: 'tw-02' } });
    fireEvent.change(view.getByLabelText('机器名称'), { target: { value: '台湾入口' } });
    expect(view.container.querySelector('.nd-ident-meta')?.textContent).toBe('tw-02 · 台湾入口');
    fireEvent.change(view.getByLabelText('DNS'), { target: { value: '1.1.1.1,,8.8.8.8' } });
    expect(dns.open).toBe(true);
    expect(view.getByLabelText('DNS').getAttribute('aria-invalid')).toBe('true');
    fireEvent.change(view.getByLabelText('Xray 管理端口'), { target: { value: '0' } });
    expect(xray.open).toBe(true);
  });

  it('OpenVPN 只追加安装参数，展示与复制一致，切换不会请求 API', async () => {
    const client = queryClient();
    client.setQueryData(['nodes'], { nodes: [{ ...agentNodes[0], token_last_used_at: null, last_poll_at: null }] });
    const fetcher = vi.fn();
    vi.stubGlobal('fetch', fetcher);
    const copied = vi.spyOn(platform, 'copyText').mockResolvedValue(true);
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall node="hk-edge-01" result={installResult()} go={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    const extension = view.getByRole('group', { name: '是否安装 OpenVPN 扩展' });
    expect(view.getByLabelText('安装命令').textContent).toBe('expired-install-command');
    fireEvent.click(within(extension).getByRole('button', { name: '安装' }));
    expect(view.getByLabelText('安装命令').textContent).toBe('expired-install-command --enable-openvpn');
    fireEvent.click(view.getByRole('button', { name: '复制命令' }));
    await waitFor(() => expect(copied).toHaveBeenCalledWith('expired-install-command --enable-openvpn'));
    expect(view.container.querySelector('.pv-flag')?.textContent).toBe('--enable-openvpn');
    fireEvent.click(within(extension).getByRole('button', { name: '不安装' }));
    expect(view.getByLabelText('安装命令').textContent).toBe('expired-install-command');
    expect(fetcher).not.toHaveBeenCalled();
  });

  it.each(['redeemed', 'expired'] as const)('%s 命令不可更改 OpenVPN 选项或复制', state => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [
        {
          ...agentNodes[0],
          token_last_used_at: state === 'redeemed' ? new Date().toISOString() : null,
          last_poll_at: null,
        },
      ],
    });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall
            node="hk-edge-01"
            result={installResult(state === 'expired' ? '2020-01-01T00:00:00Z' : null)}
            go={() => undefined}
          />
        </SessionProvider>
      </QueryClientProvider>,
    );
    const extension = view.getByRole('group', { name: '是否安装 OpenVPN 扩展' });
    expect(
      within(extension)
        .getAllByRole('button')
        .every(button => button.hasAttribute('disabled')),
    ).toBe(true);
    expect(view.queryByLabelText('安装命令')).toBeNull();
    expect(view.queryByRole('button', { name: '复制命令' })).toBeNull();
  });

  it.each([
    ['OpenVPN 2.6.14', '已安装'],
    [null, '未安装'],
  ] as const)('上线后以 Agent 上报 %s 为准，发布阶段仍是当前', (version, label) => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [
        {
          ...agentNodes[0],
          token_last_used_at: new Date().toISOString(),
          last_poll_at: new Date().toISOString(),
          runtime_versions: { openvpn: version },
        },
      ],
    });
    const go = vi.fn();
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall node="hk-edge-01" result={installResult()} go={go} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    expect(view.getByText(label)).toBeTruthy();
    expect(view.queryByRole('group', { name: '是否安装 OpenVPN 扩展' })).toBeNull();
    const stages = within(view.getByRole('list', { name: '纳管进度' })).getAllByRole('listitem');
    expect(stages[2].getAttribute('aria-current')).toBe('step');
    expect(stages[2].className).toBe('current');
    expect(view.queryByText(/在下方.*证书组/)).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '打开机器配置' }));
    expect(go).toHaveBeenCalledWith({ p: 'node', id: 'hk-edge-01', tab: 'config' });
  });

  it('在本地校验地址与端口，并在地址清空时清除无意义的 NAT 状态', () => {
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <ProvisionForm go={() => undefined} />
      </QueryClientProvider>,
    );

    expect(view.getByRole('heading', { name: '纳管机器' })).toBeTruthy();
    expect(view.getByRole('button', { name: '纳管这台机器' }).hasAttribute('disabled')).toBe(true);

    fireEvent.change(view.getByLabelText('机器 ID'), { target: { value: 'tw-edge-01' } });
    const ipv4 = view.getByLabelText('公网 IPv4', { selector: '#provision-public-ipv4' });
    fireEvent.change(ipv4, { target: { value: '203.0.113.999' } });
    expect(view.getByText('请输入完整的 IPv4 地址或主机名。')).toBeTruthy();

    const reachability = view.getByRole('group', { name: '公网 IPv4 可达方式' });
    fireEvent.change(ipv4, { target: { value: '203.0.113.9' } });
    fireEvent.click(within(reachability).getByRole('button', { name: '经 NAT' }));
    expect(within(reachability).getByRole('button', { name: '经 NAT' }).getAttribute('aria-pressed')).toBe('true');
    fireEvent.change(ipv4, { target: { value: '' } });
    expect(within(reachability).getByRole('button', { name: '经 NAT' }).hasAttribute('disabled')).toBe(true);
    expect(within(reachability).getByRole('button', { name: '直连' }).getAttribute('aria-pressed')).toBe('true');

    fireEvent.change(view.getByLabelText('WireGuard 端口'), { target: { value: '0' } });
    expect(view.getByText('请输入 1–65535 的整数。')).toBeTruthy();
  });

  it('提交归一化后的可选字段，并进入安装阶段', async () => {
    const writes: { path: string; body: Record<string, unknown> }[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: string, init?: RequestInit) => {
        writes.push({ path, body: JSON.parse(String(init?.body)) as Record<string, unknown> });
        return Response.json({
          revision_id: 13,
          node: { id: 'tw-edge-01' },
          enrollment: { token: 'once', token_prefix: 'once', expires_at: null, install_command: 'install' },
        });
      }),
    );
    const go = vi.fn();
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <ProvisionForm go={go} />
      </QueryClientProvider>,
    );

    fireEvent.change(view.getByLabelText('机器 ID'), { target: { value: 'tw-edge-01' } });
    fireEvent.change(view.getByLabelText('Xray 管理端口'), { target: { value: '' } });
    fireEvent.click(view.getByRole('button', { name: '纳管这台机器' }));

    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0]).toMatchObject({
      path: '/nodes/provision',
      body: {
        id: 'tw-edge-01',
        public_ipv4: null,
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        api_port: null,
        wg_listen_port: 51820,
        overlay: false,
      },
    });
    expect(go).toHaveBeenCalledWith(expect.objectContaining({ p: 'install', node: 'tw-edge-01' }));
  });

  it('没有可见租户时说明真正的阻塞原因，不显示伪就绪状态', () => {
    const client = queryClient();
    client.setQueryData(['tenants'], { tenants: [] });
    const view = render(
      <QueryClientProvider client={client}>
        <ProvisionForm go={() => undefined} />
      </QueryClientProvider>,
    );

    fireEvent.change(view.getByLabelText('机器 ID'), { target: { value: 'tw-edge-01' } });
    expect(view.getAllByText('当前账户配置不可用，无法纳管机器。').length).toBeGreaterThan(0);
    expect(view.queryByText(/租户/)).toBeNull();
    expect(view.getByRole('button', { name: '纳管这台机器' }).hasAttribute('disabled')).toBe(true);
    expect(view.queryByText('准备就绪')).toBeNull();
  });

  it('token 兑换后不再展示失效命令，并在启动超时后提供有确认的恢复入口', () => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [
        {
          ...agentNodes[0],
          token_prefix: 'once',
          token_last_used_at: '2020-01-01T00:00:00',
          last_poll_at: null,
        },
      ],
    });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall node="hk-edge-01" result={installResult()} go={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    expect(view.queryByText('expired-install-command')).toBeNull();
    expect(view.queryByRole('button', { name: '复制命令' })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '重新生成命令' }));
    expect(view.getByRole('dialog', { name: '重新生成安装命令' })).toBeTruthy();
    expect(view.getByText(/立即让旧 token 失效/)).toBeTruthy();
  });

  it('token 刚兑换时保留启动宽限期，不诱导用户立即重签', () => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [
        {
          ...agentNodes[0],
          token_prefix: 'once',
          token_last_used_at: new Date().toISOString(),
          last_poll_at: null,
        },
      ],
    });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall node="hk-edge-01" result={installResult()} go={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    expect(view.getByText('token 已兑换，Agent 正在启动。通常会在 15 秒内拉取配置，请先等待心跳。')).toBeTruthy();
    expect(view.queryByRole('button', { name: '重新生成命令' })).toBeNull();
  });

  it('重签过期命令后可重新选择扩展，不再沿用旧命令的到期时间', async () => {
    const client = queryClient();
    const rows = { nodes: [{ ...agentNodes[0], token_prefix: null, token_last_used_at: null, last_poll_at: null }] };
    client.setQueryData(['nodes'], rows);
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: string) =>
        Response.json(
          path.endsWith('/agent-token')
            ? {
                node_id: 'hk-edge-01',
                token: 'new-once',
                token_prefix: 'new',
                install_command: 'fresh-install-command',
              }
            : rows,
        ),
      ),
    );
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall node="hk-edge-01" result={installResult('2020-01-01T00:00:00Z')} go={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('button', { name: '重新生成命令' }));
    await waitFor(() => expect(view.getByLabelText('安装命令').textContent).toBe('fresh-install-command'));
    const extension = view.getByRole('group', { name: '是否安装 OpenVPN 扩展' });
    expect(within(extension).getByRole('button', { name: '安装' }).hasAttribute('disabled')).toBe(false);
    fireEvent.click(within(extension).getByRole('button', { name: '安装' }));
    expect(view.getByLabelText('安装命令').textContent).toBe('fresh-install-command --enable-openvpn');
    expect(view.queryByText('expired-install-command')).toBeNull();
  });

  it('过期的 enrollment 命令不会继续暴露为可执行命令', () => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [{ ...agentNodes[0], token_prefix: null, token_last_used_at: null, last_poll_at: null }],
    });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall node="hk-edge-01" result={installResult('2020-01-01T00:00:00')} go={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    expect(view.getByText('命令已过期')).toBeTruthy();
    expect(view.queryByText('expired-install-command')).toBeNull();
    expect(view.getByRole('button', { name: '重新生成命令' })).toBeTruthy();
  });
});

describe('新建链向导', () => {
  it.each(['node-certificate', 'global-site', 'custom-site'])(
    '新建 %s REALITY 接入默认使用严格 fallback 限速',
    async target => {
      draft.init(`wizard-strict-${target}`);
      draft.clear();
      const done = vi.fn();
      const view = render(
        <QueryClientProvider client={queryClient()}>
          <SessionProvider value={{ who: operator, initial }}>
            <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
          </SessionProvider>
        </QueryClientProvider>,
      );
      fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
      fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '严格限速测试' } });
      fireEvent.click(
        within(
          view.getByRole('checkbox', { name: 'VLESS · TLS/REALITY' }).closest('.wzp-card') as HTMLElement,
        ).getByRole('button', { name: '参数' }),
      );
      fireEvent.change(view.getByRole('combobox', { name: 'REALITY 目标来源' }), { target: { value: target } });
      if (target === 'custom-site') {
        fireEvent.change(view.getByLabelText('自定义 REALITY 目标'), { target: { value: 'www.example.com:443' } });
        fireEvent.change(view.getByLabelText('自定义 REALITY SNI'), { target: { value: 'www.example.com' } });
      }
      fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
      await waitFor(() => expect(done).toHaveBeenCalledOnce());
      expect(draft.ops().find(op => op.op === 'create_ingress')).toMatchObject({
        ingress: { reality: { fallback_mode: target, fallback_limits: { mode: 'strict' } } },
      });
    },
  );

  it('没有机器时仍保留纸面与配置卡，并说明如何解除阻塞', () => {
    const client = queryClient();
    client.setQueryData(['nodes'], { nodes: [] });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    expect(view.getByRole('heading', { name: '新建链' })).toBeTruthy();
    expect(view.getByText('先去「机器」纳管一台，再回来选择入口。')).toBeTruthy();
    expect(view.container.querySelectorAll('.nd-paper')).toHaveLength(1);
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
  });

  function mount(client = queryClient()) {
    return render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );
  }

  it('首次机器数据返回前等待完整依赖，再显示建链表单', async () => {
    const client = queryClient();
    client.removeQueries({ queryKey: ['nodes'] });
    let finishNodes!: (response: Response) => void;
    vi.stubGlobal(
      'fetch',
      vi.fn(
        () =>
          new Promise<Response>(resolve => {
            finishNodes = resolve;
          }),
      ),
    );
    const view = mount(client);
    expect(view.queryByRole('heading', { name: '新建链' })).toBeNull();
    expect(view.queryByRole('textbox', { name: '链名称' })).toBeNull();

    await act(async () => finishNodes(Response.json({ nodes: agentNodes })));
    await waitFor(() => expect(view.getByRole('textbox', { name: '链名称' })).toBeTruthy());
    expect(view.container.querySelector<HTMLFieldSetElement>('.pv-chain-fields')?.disabled).toBe(false);
  });

  it('机器状态后台轮询不禁用表单、不切换页脚，并保留输入和展开状态', async () => {
    const client = queryClient();
    let finishNodes!: (response: Response) => void;
    const fetcher = vi.fn((path: string) => {
      if (path !== '/nodes/agent-state') throw new Error(`unexpected request: ${path}`);
      return new Promise<Response>(resolve => {
        finishNodes = resolve;
      });
    });
    vi.stubGlobal('fetch', fetcher);
    const view = mount(client);
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    const name = view.getByRole('textbox', { name: '链名称' }) as HTMLInputElement;
    const port = view.getByRole('textbox', { name: 'AnyTLS 监听端口' }) as HTMLInputElement;
    fireEvent.change(name, { target: { value: '轮询时继续编辑' } });
    fireEvent.change(port, { target: { value: '25444' } });
    const anyTlsCard = view.getByRole('checkbox', { name: 'AnyTLS' }).closest('.wzp-card') as HTMLElement;
    fireEvent.click(within(anyTlsCard).getByRole('button', { name: '参数' }));
    const padding = view.getByRole('combobox', { name: 'Padding' });
    const paper = view.container.querySelector('.nd-paper');
    const fields = view.container.querySelector<HTMLFieldSetElement>('.pv-chain-fields')!;
    const footer = view.container.querySelector('.pv-state')!;
    const footerText = footer.textContent;

    for (let cycle = 0; cycle < 2; cycle += 1) {
      await act(async () => {
        void client.refetchQueries({ queryKey: ['nodes'] });
      });
      expect(client.getQueryState(['nodes'])).toMatchObject({ status: 'success', fetchStatus: 'fetching' });
      fireEvent.change(name, { target: { value: `轮询中编辑 ${cycle}` } });
      expect(view.container.querySelector('.nd-paper')).toBe(paper);
      expect(fields.disabled).toBe(false);
      expect(name.matches(':disabled')).toBe(false);
      expect(port.matches(':disabled')).toBe(false);
      expect(footer.textContent).toBe(footerText);
      expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false);
      await act(async () => finishNodes(Response.json({ nodes: agentNodes })));
      await waitFor(() => expect(client.getQueryState(['nodes'])?.fetchStatus).toBe('idle'));
      expect(view.getByRole('textbox', { name: '链名称' })).toBe(name);
      expect(name.value).toBe(`轮询中编辑 ${cycle}`);
      expect(port.value).toBe('25444');
      expect(view.getByRole('combobox', { name: 'Padding' })).toBe(padding);
      expect(footer.textContent).toBe(footerText);
    }
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(fetcher.mock.calls.every(([path]) => path === '/nodes/agent-state')).toBe(true);
  });

  it('机器状态刷新失败时保留表面并显示错误，阻止加入草稿', async () => {
    const client = queryClient();
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ error: '机器状态读取失败' }, { status: 503 })),
    );
    const view = mount(client);
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    const name = view.getByRole('textbox', { name: '链名称' }) as HTMLInputElement;
    fireEvent.change(name, { target: { value: '失败时保留的链' } });
    const paper = view.container.querySelector('.nd-paper');

    await act(async () => {
      await client.refetchQueries({ queryKey: ['nodes'] });
    });
    await waitFor(() => expect(view.getByText('机器状态读取失败')).toBeTruthy());
    expect(view.getByRole('button', { name: '重试读取配置' })).toBeTruthy();
    expect(view.container.querySelector('.nd-paper')).toBe(paper);
    expect(view.getByRole('textbox', { name: '链名称' })).toBe(name);
    expect(name.value).toBe('失败时保留的链');
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
  });

  it('入口分组仅默认选中，菜单保留完整默认顺序，切换后写入所选分组', async () => {
    draft.init('wizard-group-default-switch');
    draft.clear();
    const client = queryClient();
    const cached = client.getQueryData<ConsoleSnapshot>(['snapshot'])!;
    const template = cached.snapshot.apps[0];
    client.setQueryData(['snapshot'], {
      ...cached,
      snapshot: {
        ...cached.snapshot,
        apps: [
          { ...template, id: 'group-last', label: '丙组' },
          template,
          { ...template, id: 'group-first', label: '甲组' },
        ],
      },
    });
    const view = mount(client);
    const group = view.getByRole('combobox', { name: '分组' }) as HTMLSelectElement;
    const options = [...group.options].map(option => option.value);
    expect(group.value).toBe('global');
    expect(options).toEqual(['group-last', 'global', 'group-first', ' :new-app:']);
    expect((view.getByRole('textbox', { name: '链名称' }) as HTMLInputElement).value).toBe('');
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '手动命名的链' } });
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(group, { target: { value: 'group-first' } });
    expect(group.value).toBe('group-first');
    expect([...group.options].map(option => option.value)).toEqual(options);
    expect(view.getByText('手动命名的链 · 甲组')).toBeTruthy();
    expect((view.getByRole('textbox', { name: '链名称' }) as HTMLInputElement).value).toBe('手动命名的链');
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() =>
      expect(draft.ops().find(op => op.op === 'create_ingress')).toMatchObject({ app_id: 'group-first' }),
    );
    expect(draft.ops().find(op => op.op === 'create_chain')).toMatchObject({
      app_id: 'group-first',
      chain: { name: '手动命名的链' },
    });
    expect(draft.ops().some(op => op.op === 'create_app')).toBe(false);
  });

  it('从分组入口创建链仍可新建分组，并把链加入新分组', async () => {
    draft.init('wizard-group-default-new');
    draft.clear();
    const view = mount();
    const group = view.getByRole('combobox', { name: '分组' });
    fireEvent.change(group, { target: { value: ' :new-app:' } });
    fireEvent.change(view.getByRole('textbox', { name: '新分组名称' }), { target: { value: '自选分组' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '新分组里的链' } });
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    expect(view.getByText('新分组里的链 · 自选分组')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(draft.ops().some(op => op.op === 'create_ingress')).toBe(true));
    const newGroup = draft.ops().find(op => op.op === 'create_app');
    expect(newGroup).toMatchObject({ app: { label: '自选分组' } });
    if (newGroup?.op !== 'create_app') throw new Error('expected group creation before chain creation');
    expect(draft.ops().find(op => op.op === 'create_chain')).toMatchObject({ app_id: newGroup.app.id });
  });

  it('先解释阻塞原因，选择入口后在对应配置卡显示当前值', () => {
    const view = mount();
    expect(view.getByText('选择入口节点')).toBeTruthy();
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);

    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    expect(view.getByText('3 种 · 端口自动避让')).toBeTruthy();
    expect(view.getByText('未命名 · 全球加速')).toBeTruthy();
    expect(view.getByRole('textbox', { name: '链名称' }).getAttribute('placeholder')).toBe('给这条链起个名字');
    expect((view.getByRole('textbox', { name: '链名称' }) as HTMLInputElement).value).toBe('');
    expect(view.queryByLabelText('新链摘要')).toBeNull();
    expect(view.container.querySelectorAll('.nd-paper')).toHaveLength(1);
    expect(view.container.querySelectorAll('.wz-head, .wz-foot')).toHaveLength(0);
    expect(view.queryByLabelText('出口地区标识')).toBeNull();
    expect(view.queryByText('出口地区标识')).toBeNull();
    expect(view.getByText('填写链名称')).toBeTruthy();
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '亚太分流' } });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false);
  });

  it('链名称默认留空，选择入口或修改路径不自动填充，手动命名和清空后保留输入', () => {
    const view = mount();
    const name = view.getByRole('textbox', { name: '链名称' }) as HTMLInputElement;
    expect(name.value).toBe('');
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    expect(name.value).toBe('');
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), {
      target: { value: 'sg-relay-01' },
    });
    expect(name.value).toBe('');
    fireEvent.click(view.getByRole('button', { name: '把 新加坡中转 移出本链' }));
    expect(name.value).toBe('');
    fireEvent.click(view.getByRole('button', { name: '换一台当入口' }));
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'sg-relay-01' },
    });
    expect(name.value).toBe('');
    fireEvent.click(view.getByRole('button', { name: '换一台当入口' }));
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });

    fireEvent.change(name, { target: { value: '手动链名' } });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), {
      target: { value: 'sg-relay-01' },
    });
    expect(name.value).toBe('手动链名');
    fireEvent.change(name, { target: { value: '' } });
    fireEvent.click(view.getByRole('button', { name: '把 新加坡中转 移出本链' }));
    expect(name.value).toBe('');
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
  });

  it('新增规则默认 geosite，保留原匹配菜单顺序，并保持终端规则置底', () => {
    const view = mount();
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '规则校验测试' } });
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    const match = view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }) as HTMLSelectElement;
    expect(match.value).toBe('geosite');
    expect([...match.options].map(option => option.value)).toEqual([
      'any',
      'sniffing_failed',
      'geosite',
      'geoip',
      'domain_suffix',
      'domain_keyword',
      'domain_regex',
      'ip_cidr',
      'port',
      'network',
    ]);
    const values = view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }) as HTMLInputElement;
    expect(values.value).toBe('');
    expect(values.placeholder).toContain('netflix');
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    fireEvent.change(values, { target: { value: 'netflix' } });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false);

    fireEvent.change(match, { target: { value: 'domain_keyword' } });
    expect(values.value).toBe('');
    expect(values.placeholder).toContain('google');
    expect((view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }) as HTMLSelectElement).value).toBe('egress');
    fireEvent.change(match, { target: { value: 'sniffing_failed' } });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    const matches = view.getAllByRole('combobox', { name: /香港边缘 第 \d+ 条匹配条件/ }) as HTMLSelectElement[];
    expect(matches.map(select => select.value)).toEqual(['geosite', 'sniffing_failed', 'any']);
    expect(matches[0].querySelector<HTMLOptionElement>('option[value="sniffing_failed"]')?.disabled).toBe(true);
    expect(matches.at(-1)?.disabled).toBe(true);
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
  });

  it('没有 geosite 选项时默认域名关键词，并保留其菜单位置', () => {
    vi.spyOn(ruleEditor, 'MATCH_KINDS', 'get').mockReturnValue(
      ruleEditor.MATCH_KINDS.filter(kind => kind.t !== 'geosite'),
    );
    const view = mount();
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '关键词校验测试' } });
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    const match = view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }) as HTMLSelectElement;
    expect(match.value).toBe('domain_keyword');
    expect([...match.options].map(option => option.value)).toEqual([
      'any',
      'sniffing_failed',
      'geoip',
      'domain_suffix',
      'domain_keyword',
      'domain_regex',
      'ip_cidr',
      'port',
      'network',
    ]);
    const values = view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }) as HTMLInputElement;
    expect(values.placeholder).toContain('google');
    fireEvent.change(values, { target: { value: 'google' } });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false);
  });

  it('接入协议沿用详情页的逐行选择样式，图标与端口编辑不干扰开关', () => {
    const view = mount();
    const cards = [...view.container.querySelectorAll('.wzp-card')];
    expect(cards).toHaveLength(5);
    expect(cards.every(card => card.classList.contains('protocol-choice'))).toBe(true);
    expect(cards.every(card => card.querySelector('.protocol-choice-copy .protocol-choice-icon'))).toBe(true);
    expect(cards[0].querySelector('.protocol-choice-icon polygon')).toBeTruthy();
    expect(cards[3].querySelector('.protocol-choice-icon polygon[fill="#ffbc00"]')).toBeTruthy();
    expect(cards.map(card => card.querySelector('.note')?.textContent)).toEqual([
      '支持 TCP / XHTTP，以 TLS 加密或 REALITY 伪装保护连接，适用于通用代理接入。',
      'VLESS 原生加密，适用于无证书加密接入。',
      '通过分包填充缓解嵌套 TLS 特征，以连接复用降低握手开销，适用于短连接密集场景。',
      '基于 QUIC / UDP，针对高延迟与丢包链路优化吞吐，适用于弱网场景。',
      'Telegram 原生支持的专用代理。',
    ]);

    const anyTls = view.getByRole('checkbox', { name: 'AnyTLS' }) as HTMLInputElement;
    fireEvent.click(anyTls.closest('.wzp-card')!.querySelector('.protocol-choice-copy')!);
    expect(anyTls.checked).toBe(false);
    fireEvent.click(anyTls);
    expect(anyTls.checked).toBe(true);
    fireEvent.click(view.getByRole('textbox', { name: 'AnyTLS 监听端口' }));
    expect(anyTls.checked).toBe(true);
    expect(cards.every(card => card.querySelector('.protocol-choice-status') === null)).toBe(true);

    const encryption = view.getByRole('checkbox', { name: 'VLESS · Encryption' }) as HTMLInputElement;
    expect(encryption.checked).toBe(false);
    const encryptionCard = encryption.closest('.wzp-card') as HTMLElement;
    fireEvent.click(within(encryptionCard).getByRole('button', { name: '参数' }));
    fireEvent.change(within(encryptionCard).getByRole('combobox', { name: '握手档位' }), {
      target: { value: 'native' },
    });
    expect(encryption.checked).toBe(false);
    expect((within(encryptionCard).getByRole('combobox', { name: '握手档位' }) as HTMLSelectElement).value).toBe(
      'native',
    );
  });

  it('所有接入协议都可先选择，切换入口不会重置协议选择', () => {
    const view = mount();
    const protocols = ['VLESS · TLS/REALITY', 'VLESS · Encryption', 'AnyTLS', 'Hysteria 2', 'MTProxy'];
    const choices = protocols.map(name => view.getByRole('checkbox', { name }) as HTMLInputElement);
    const selected = choices.map(choice => {
      expect(choice.disabled).toBe(false);
      const initial = choice.checked;
      fireEvent.click(choice);
      expect(choice.checked).toBe(!initial);
      return choice.checked;
    });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);

    for (const nodeId of ['hk-edge-01', 'sg-relay-01']) {
      fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: nodeId } });
      expect(choices.map(choice => choice.checked)).toEqual(selected);
      expect(choices.every(choice => !choice.disabled)).toBe(true);
      fireEvent.click(view.getByRole('button', { name: '换一台当入口' }));
      expect(choices.map(choice => choice.checked)).toEqual(selected);
      expect(choices.every(choice => !choice.disabled)).toBe(true);
    }
  });

  it('先选择 MTProxy 和端口，再选择入口，并把端口和投影一同写入草稿', async () => {
    draft.init(`wizard-mtproxy-${Math.random()}`);
    draft.clear();
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    const mtproxy = view.getByRole('checkbox', { name: 'MTProxy' }) as HTMLInputElement;
    expect(mtproxy.disabled).toBe(false);
    fireEvent.click(mtproxy);
    fireEvent.change(view.getByRole('textbox', { name: 'MTProxy 监听端口' }), { target: { value: '28812' } });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);

    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    expect(mtproxy.disabled).toBe(false);
    expect(mtproxy.checked).toBe(true);
    expect((view.getByRole('textbox', { name: 'MTProxy 监听端口' }) as HTMLInputElement).value).toBe('28812');
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: 'Telegram 接入' } });
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));

    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    expect(draft.ops().find(op => op.op === 'create_ingress')).toMatchObject({
      ingress: {
        wires: { mtproto: { port: 28812 } },
        projection: { mtproto: {} },
      },
    });
  });

  it.each([
    [null, false, '当前机器没有配置公网 IPv4，不能启用 MTProxy'],
    ['203.0.113.10', true, '当前机器的公网 IPv4 未直接配置在网卡上，不能启用 MTProxy'],
  ])('不兼容入口仍可选择 MTProxy，创建校验提示原因：%s / NAT=%s', (publicIpv4, nat, reason) => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [{ ...agentNodes[0], public_ipv4: publicIpv4, public_ipv4_nat: nat }, agentNodes[1]],
    });
    const view = mount(client);
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: 'Telegram 接入' } });
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    const mtproxy = view.getByRole('checkbox', { name: 'MTProxy' }) as HTMLInputElement;
    expect(mtproxy.disabled).toBe(false);
    expect(view.queryByRole('alert')).toBeNull();
    fireEvent.click(mtproxy);
    expect(mtproxy.checked).toBe(true);
    expect(view.getByRole('alert').textContent).toBe(reason);
    expect(view.getByText('Telegram 原生支持的专用代理。')).toBeTruthy();
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    fireEvent.click(mtproxy);
    expect(view.queryByRole('alert')).toBeNull();
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false);
    fireEvent.click(mtproxy);
    fireEvent.click(view.getByRole('button', { name: '换一台当入口' }));
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'sg-relay-01' },
    });
    expect(mtproxy.checked).toBe(true);
    expect(view.queryByRole('alert')).toBeNull();
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false);
  });

  it('选择用户不依赖入口节点，之后选择入口仍保留授权选择', () => {
    const view = mount();
    expect(view.queryByText('先选择入口节点，再添加可用用户。')).toBeNull();
    const alice = view.getByRole('button', { name: /^alice(?:\s|$)/ });
    fireEvent.click(alice);
    expect(alice.getAttribute('aria-pressed')).toBe('true');
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    expect(view.getByRole('button', { name: /^alice(?:\s|$)/ }).getAttribute('aria-pressed')).toBe('true');
  });

  it('授权用户平铺且不暴露归属，搜索和全选只作用于当前可见用户', () => {
    const client = queryClient();
    client.setQueryData(['tenants'], {
      tenants: [
        { id: 'platform', name: '平台', node_count: 2, user_count: 3, operator_count: 1 },
        { id: 'platform.design', name: '设计团队', node_count: 0, user_count: 1, operator_count: 0 },
      ],
    });
    const cachedUsers = client.getQueryData<{ users: object[] }>(['users']);
    client.setQueryData(['users'], {
      users: [
        ...(cachedUsers?.users ?? []),
        { tenant_id: 'platform.design', id: 'carol', status: 'active', created_at: '', created_revision: 1 },
      ],
    });
    const view = mount(client);
    expect(view.getByText('授权用户')).toBeTruthy();
    expect(view.queryByText('谁能用')).toBeNull();
    expect(view.queryByRole('region', { name: '设计团队的用户' })).toBeNull();
    expect(view.queryByText('设计团队')).toBeNull();
    expect(view.queryByText('platform.design')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: /^新增用户/ }));
    expect(view.queryByRole('combobox', { name: /租户/ })).toBeNull();
    fireEvent.click(view.getByRole('group', { name: '新增用户' }).querySelector('button')!);
    const alice = view.getByRole('button', { name: /^alice(?:\s|$)/ });
    const avatar = alice.querySelector('.user-generated-avatar');
    expect(avatar).toBeTruthy();
    expect(avatar?.querySelector('.user-avatar-mark')).toBeTruthy();
    expect(avatar?.querySelector('img')).toBeNull();
    fireEvent.click(alice);
    const search = view.getByRole('searchbox', { name: /搜索/ });
    fireEvent.change(search, { target: { value: '设计团队' } });
    expect(view.queryByRole('button', { name: /^carol(?:\s|$)/ })).toBeNull();
    expect(view.getByText('没有匹配的用户')).toBeTruthy();
    fireEvent.change(search, { target: { value: 'CAROL' } });
    expect(view.getByRole('button', { name: /^carol(?:\s|$)/ })).toBeTruthy();
    expect(view.queryByRole('button', { name: /^alice(?:\s|$)/ })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '全选' }));
    expect(view.getByRole('button', { name: /^carol(?:\s|$)/ }).getAttribute('aria-pressed')).toBe('true');

    fireEvent.change(search, { target: { value: 'BOB' } });
    const bob = view.getByRole('button', { name: /^bob(?:\s|$)/ });
    expect(bob.getAttribute('aria-pressed')).toBe('false');
    fireEvent.click(bob);
    fireEvent.change(search, { target: { value: 'no-such-user' } });
    expect(view.getByText('没有匹配的用户')).toBeTruthy();
    fireEvent.change(search, { target: { value: '' } });
    for (const id of ['alice', 'bob', 'carol']) {
      expect(view.getByRole('button', { name: new RegExp(`^${id}(?:\\s|$)`) }).getAttribute('aria-pressed')).toBe(
        'true',
      );
    }
    expect(view.getByRole('button', { name: /legacy-user/ }).hasAttribute('disabled')).toBe(true);
    fireEvent.click(view.getByRole('button', { name: /^(?:清空|全不选)$/ }));
    for (const id of ['alice', 'bob', 'carol']) {
      expect(view.getByRole('button', { name: new RegExp(`^${id}(?:\\s|$)`) }).getAttribute('aria-pressed')).toBe(
        'false',
      );
    }
  });

  it('搜索中新增用户后清空筛选，并在创建成功后自动选中', async () => {
    const client = queryClient();
    const cachedSnapshot = client.getQueryData(['snapshot']);
    const cachedRevisions = client.getQueryData(['revisions']);
    const fetcher = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/users' && init?.method === 'POST') {
        return Response.json(
          {
            revision_id: 13,
            user: {
              tenant_id: 'platform',
              id: 'carol',
              status: 'active',
              created_at: '2026-10-19T00:00:00Z',
              created_revision: 13,
            },
          },
          { status: 201 },
        );
      }
      if (path.startsWith('/revisions')) return Response.json(cachedRevisions);
      if (path === '/model/snapshot') return Response.json(cachedSnapshot);
      return Response.json({ error: `unexpected request: ${path}` }, { status: 500 });
    });
    vi.stubGlobal('fetch', fetcher);
    const view = mount(client);

    expect(view.getByRole('group', { name: '选择可用用户' })).toBeTruthy();
    const search = view.getByRole('searchbox', { name: /搜索/ }) as HTMLInputElement;
    fireEvent.change(search, { target: { value: 'alice' } });
    fireEvent.click(view.getByRole('button', { name: /^新增用户/ }));
    fireEvent.change(view.getByRole('textbox', { name: '新用户 ID' }), { target: { value: 'carol' } });
    fireEvent.click(view.getByRole('button', { name: '新增并选中' }));

    await waitFor(() =>
      expect(view.getByRole('button', { name: /^carol(?:\s|$)/ }).getAttribute('aria-pressed')).toBe('true'),
    );
    const createCall = fetcher.mock.calls.find(([path, init]) => path === '/users' && init?.method === 'POST');
    expect(createCall).toBeTruthy();
    expect(JSON.parse(String(createCall?.[1]?.body))).toEqual({ tenant_id: 'platform', id: 'carol' });
    expect(search.value).toBe('');
    expect(view.queryByRole('group', { name: '新增用户' })).toBeNull();
  });

  it('新增用户推进修订时保留已挂载表面和输入，等待修订和编译完成后才能加入草稿', async () => {
    draft.init(`wizard-user-refresh-${Math.random()}`);
    draft.clear();
    const client = queryClient();
    const cachedSnapshot = client.getQueryData(['snapshot']);
    let finishRevisions!: (response: Response) => void;
    const refreshedRevisions = new Promise<Response>(resolve => {
      finishRevisions = resolve;
    });
    let finishCompile!: (response: Response) => void;
    const refreshedCompile = new Promise<Response>(resolve => {
      finishCompile = resolve;
    });
    const fetcher = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/users' && init?.method === 'POST') {
        return Response.json({
          revision_id: 13,
          user: { tenant_id: 'platform', id: 'carol', status: 'active', created_at: '', created_revision: 13 },
        });
      }
      if (path.startsWith('/revisions')) return refreshedRevisions;
      if (path === '/model/snapshot') return Response.json(cachedSnapshot);
      if (path === '/compile/13') return refreshedCompile;
      return Response.json({ error: `unexpected request: ${path}` }, { status: 500 });
    });
    vi.stubGlobal('fetch', fetcher);
    const view = mount(client);
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), {
      target: { value: 'sg-relay-01' },
    });
    const name = view.getByRole('textbox', { name: '链名称' });
    const port = view.getByRole('textbox', { name: 'AnyTLS 监听端口' });
    fireEvent.change(name, { target: { value: '刷新中保留的链' } });
    fireEvent.change(port, { target: { value: '25444' } });
    fireEvent.click(view.getByRole('button', { name: /^alice(?:\s|$)/ }));
    const anyTlsCard = view.getByRole('checkbox', { name: 'AnyTLS' }).closest('.wzp-card') as HTMLElement;
    fireEvent.click(within(anyTlsCard).getByRole('button', { name: '参数' }));
    const padding = view.getByRole('combobox', { name: 'Padding' });
    fireEvent.click(view.getByRole('button', { name: /^新增用户/ }));
    fireEvent.change(view.getByRole('textbox', { name: '新用户 ID' }), { target: { value: 'carol' } });
    fireEvent.click(view.getByRole('button', { name: '新增并选中' }));

    await waitFor(() => expect(fetcher.mock.calls.some(([path]) => path.startsWith('/revisions'))).toBe(true));
    expect(client.getQueryData(['revisions'])).toMatchObject({ current_revision: 12 });
    expect(client.getQueryState(['revisions'])).toMatchObject({ status: 'success', fetchStatus: 'fetching' });
    expect(view.getByRole('textbox', { name: '链名称' })).toBe(name);
    expect((name as HTMLInputElement).value).toBe('刷新中保留的链');
    expect(view.getByRole('textbox', { name: 'AnyTLS 监听端口' })).toBe(port);
    expect((port as HTMLInputElement).value).toBe('25444');
    expect(view.getByRole('combobox', { name: 'Padding' })).toBe(padding);
    expect(view.getByText('等待最新配置加载完成')).toBeTruthy();
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    expect(fetcher.mock.calls.some(([path]) => path === '/compile/13')).toBe(false);
    expect(draft.ops()).toEqual([]);

    finishRevisions(Response.json({ current_revision: 13, revisions: [{ id: 13 }] }));
    await waitFor(() => expect(fetcher.mock.calls.some(([path]) => path === '/compile/13')).toBe(true));
    expect(view.getByRole('textbox', { name: '链名称' })).toBe(name);
    expect((name as HTMLInputElement).value).toBe('刷新中保留的链');
    expect(view.getByRole('textbox', { name: 'AnyTLS 监听端口' })).toBe(port);
    expect((port as HTMLInputElement).value).toBe('25444');
    expect(view.getByRole('combobox', { name: 'Padding' })).toBe(padding);
    expect(view.getByRole('button', { name: /^alice(?:\s|$)/ }).getAttribute('aria-pressed')).toBe('true');
    expect(view.getByRole('button', { name: /^carol(?:\s|$)/ }).getAttribute('aria-pressed')).toBe('true');
    expect(view.getByText('等待最新配置加载完成')).toBeTruthy();
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    expect(draft.ops()).toEqual([]);

    finishCompile(Response.json({ diagnostics: [], system: { nodes: [] } }));
    await waitFor(() => expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false));
    expect(view.getByRole('textbox', { name: '链名称' })).toBe(name);
    expect((port as HTMLInputElement).value).toBe('25444');
    expect(view.getByRole('combobox', { name: 'Padding' })).toBe(padding);
  });

  it('入口租户变化时保留已选用户并说明不可授权，切回后恢复', () => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [
        ...agentNodes,
        { ...agentNodes[0], node_id: 'branch-edge-01', tenant_id: 'platform.branch', name: '分支入口' },
      ],
    });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('button', { name: /^alice(?:\s|$)/ }));
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'branch-edge-01' },
    });
    const unavailable = view.getByRole('button', { name: /^alice/ });
    expect(unavailable.getAttribute('aria-pressed')).toBe('true');
    expect(unavailable.getAttribute('title')).toContain('不在当前入口的可授权范围内');
    expect(view.getByText('alice 当前不可授权，本次不会写入。')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '换一台当入口' }));
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    expect(view.getByRole('button', { name: /^alice(?:\s|$)/ }).getAttribute('aria-pressed')).toBe('true');
    expect(view.queryByText('alice 当前不可授权，本次不会写入。')).toBeNull();
  });

  it('目标菜单沿用线路规则的分组、搜索与两级动作', () => {
    const client = queryClient();
    const cached = client.getQueryData<{ snapshot: object }>(['snapshot']);
    client.setQueryData(['snapshot'], {
      ...cached,
      snapshot: {
        ...cached?.snapshot,
        external_outbounds: [
          {
            id: 'warp-8f3a-2d71',
            tenant: 'platform',
            name: 'Cloudflare WARP',
            address: 'engage.cloudflareclient.com',
            port: 2408,
            protocol: {
              t: 'warp',
              v: { http_version: 'h3', remote_dns: ['1.1.1.1', '1.0.0.1'] },
            },
            security: { t: 'none' },
            bindings: [],
          },
        ],
      },
    });
    const view = mount(client);
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), {
      target: { value: 'forward' },
    });
    expect(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }).getAttribute('aria-expanded')).toBe('true');
    const menu = openTargetMenu('香港边缘 第 1 条转发目标');
    expect([...menu.querySelectorAll('.external-target-menu-label')].map(item => item.textContent)).toEqual([
      'BROCADE 节点',
      '在机器上新建本链监听',
      '外部出站',
    ]);
    expect(within(menu as HTMLElement).getByPlaceholderText('搜索机器或代理出站')).toBeTruthy();
    const warp = within(menu as HTMLElement).getByRole('button', { name: /WARPCloudflare WARP共享资源/ });
    expect(warp.classList.contains('external-target-option-select')).toBe(true);
    expect(warp.parentElement?.classList.contains('external-target-option')).toBe(true);
    expect(within(menu as HTMLElement).getByRole('button', { name: '打开隧道 Cloudflare WARP' })).toBeTruthy();
    expect(within(menu as HTMLElement).getByRole('button', { name: /自定义.*手动设置 IP、端口和协议/ })).toBeTruthy();
    fireEvent.click(within(menu as HTMLElement).getByRole('button', { name: /复用其他链的监听/ }));
    expect(within(menu as HTMLElement).getByText('复用其他链的监听')).toBeTruthy();
    fireEvent.click(within(menu as HTMLElement).getByRole('button', { name: '返回目标列表' }));
    expect(within(menu as HTMLElement).getByPlaceholderText('搜索机器或代理出站')).toBeTruthy();
  });

  it('自定义目标手动设置 IPv6 和端口，与新链规则一起加入草稿', async () => {
    draft.init(`wizard-custom-${Math.random()}`);
    draft.clear();
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '自定义目标' } });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'domain_suffix' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.com' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), { target: { value: 'forward' } });
    const openCustom = () => {
      openTargetMenu('香港边缘 第 1 条转发目标');
      fireEvent.click(within(document.body).getByRole('button', { name: /自定义.*手动设置 IP、端口和协议/ }));
    };
    openCustom();
    expect(view.getByRole('button', { name: '创建并选中' }).hasAttribute('disabled')).toBe(true);
    fireEvent.click(within(view.getByRole('dialog', { name: '配置代理出站' })).getByRole('button', { name: '取消' }));
    await waitFor(() => expect(document.body.querySelector('.external-outbound-drawer')).toBeNull());
    expect(draft.ops()).toEqual([]);
    expect(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }).textContent).toContain('新加坡中转');
    openCustom();
    const form = within(view.getByRole('dialog', { name: '配置代理出站' }));
    expect(form.queryByRole('textbox', { name: '分享链接' })).toBeNull();
    fireEvent.click(form.getByRole('button', { name: 'SOCKS5' }));
    fireEvent.change(form.getByRole('textbox', { name: '服务器地址' }), { target: { value: '2001:db8::40' } });
    fireEvent.change(form.getByRole('spinbutton', { name: '服务器端口' }), { target: { value: '18080' } });
    fireEvent.change(form.getByRole('combobox', { name: '目标域名解析' }), { target: { value: 'use_ipv6' } });
    fireEvent.click(form.getByRole('button', { name: '创建并选中' }));
    await waitFor(() => expect(view.queryByRole('dialog', { name: '配置代理出站' })).toBeNull());
    expect(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }).textContent).toContain('新代理出站');
    expect(draft.ops()).toEqual([]);
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    const outbound = draft.ops().find(op => op.op === 'upsert_external_outbound');
    expect(outbound).toMatchObject({
      outbound: {
        tenant_id: 'platform',
        address: '2001:db8::40',
        port: 18080,
        protocol: { t: 'socks5' },
        target_strategy: 'use_ipv6',
      },
    });
    if (outbound?.op !== 'upsert_external_outbound') throw new Error('没有自定义代理出站');
    expect(draft.ops().find(op => op.op === 'put_step')).toMatchObject({
      step: { rules: [{ a: { t: 'proxy', outbound: outbound.outbound.id } }, { a: { t: 'egress' } }] },
    });
  });

  it('复用监听子菜单把引用写入新链规则', async () => {
    draft.init(`wizard-listener-${Math.random()}`);
    draft.clear();
    const client = queryClient();
    const cached = client.getQueryData<{ snapshot: { apps: object[] } }>(['snapshot']);
    client.setQueryData(['snapshot'], {
      ...cached,
      snapshot: {
        ...cached?.snapshot,
        apps: [
          ...(cached?.snapshot.apps ?? []),
          {
            id: 'shared',
            label: '共享出口',
            chains: [{ id: 'shared-chain', name: '共享链', tenant: 'platform' }],
            ingresses: [
              {
                id: 'shared-in',
                chain: 'shared-chain',
                node: 'sg-relay-01',
                bind: '0.0.0.0',
                port: 443,
                projection: {},
                guard: {
                  no_private: true,
                  no_bittorrent: true,
                  no_mail: true,
                  no_udp_amplification: true,
                  tcp_and_quic_only: false,
                  cn_source_block: 'off',
                },
                identity: { public_key: 'public', short_ids: ['0123abcd'] },
                wires: { vless: { kind: 'vless-reality' } },
              },
            ],
            steps: [
              {
                chain: 'shared-chain',
                node: 'hk-edge-01',
                accept: { uuid: 'shared-uuid', label: 'shared-chain@hk-edge-01' },
                hop_in: { port: 21000, security: { t: 'none' } },
                rules: [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
              },
            ],
            fronts: [],
            grants: [],
          },
        ],
      },
    });
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '复用监听测试' } });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'domain_suffix' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.com' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), { target: { value: 'forward' } });
    openTargetMenu('香港边缘 第 1 条转发目标');
    fireEvent.click(within(document.body).getByRole('button', { name: /复用其他链的监听/ }));
    fireEvent.click(within(document.body).getByRole('button', { name: /共享链.*1 个监听端点/ }));
    fireEvent.click(within(document.body).getByRole('button', { name: /本机香港边缘 · TCP 21000.*0 处引用/ }));
    expect(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }).textContent).toContain('引用');
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    expect(draft.ops().find(op => op.op === 'put_step')).toMatchObject({
      step: {
        rules: [
          { a: { t: 'reuse_listener', listener: { chain: 'shared-chain', node: 'hk-edge-01' } } },
          { a: { t: 'egress' } },
        ],
      },
    });
  });

  it('展开协议卡的关键参数会写入接入面，缺半边带宽会阻止提交', async () => {
    draft.init(`wizard-protocol-params-${Math.random()}`);
    draft.clear();
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '参数测试' } });
    fireEvent.click(
      within(view.getByRole('checkbox', { name: 'AnyTLS' }).closest('.wzp-card') as HTMLElement).getByRole('button', {
        name: '参数',
      }),
    );
    fireEvent.change(view.getByRole('combobox', { name: 'Padding' }), { target: { value: 'custom' } });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    fireEvent.change(view.getByRole('textbox', { name: 'AnyTLS 自定义 Padding' }), {
      target: { value: 'stop=4\n0=20-30' },
    });
    fireEvent.click(
      within(view.getByRole('checkbox', { name: 'Hysteria 2' }).closest('.wzp-card') as HTMLElement).getByRole(
        'button',
        { name: '参数' },
      ),
    );
    fireEvent.change(view.getByRole('textbox', { name: 'Hysteria 2 上行带宽' }), {
      target: { value: '200 mbps' },
    });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    fireEvent.change(view.getByRole('textbox', { name: 'Hysteria 2 下行带宽' }), {
      target: { value: '500 mbps' },
    });
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    const ingress = draft.ops().find(op => op.op === 'create_ingress');
    expect(ingress).toMatchObject({
      ingress: {
        reality: { fallback_limits: { mode: 'strict' } },
        wires: {
          anytls: { padding_scheme: ['stop=4', '0=20-30'] },
          hysteria2: { bandwidth: { up: '200 mbps', down: '500 mbps' } },
        },
      },
    });
  });

  it('协议参数显示实际证书和伪装目标，并准确说明会话票据及空带宽行为', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    const protocol = (name: string) => view.getByRole('checkbox', { name }).closest('.wzp-card') as HTMLElement;
    const expand = (name: string) => fireEvent.click(within(protocol(name)).getByRole('button', { name: '参数' }));

    expand('VLESS · TLS/REALITY');
    fireEvent.change(view.getByRole('combobox', { name: 'REALITY 目标来源' }), {
      target: { value: 'global-site' },
    });
    expect(within(protocol('VLESS · TLS/REALITY')).getByText('www.cloudflare.com:443', { exact: true })).toBeTruthy();
    expect(within(protocol('VLESS · TLS/REALITY')).getByText('www.cloudflare.com', { exact: true })).toBeTruthy();

    expand('VLESS · Encryption');
    expect(protocol('VLESS · Encryption').textContent).toMatch(/会话票据有效期\s*600s/);
    expand('AnyTLS');
    expect(within(protocol('AnyTLS')).getByText('hk.nodes.example.net', { exact: true })).toBeTruthy();
    expand('Hysteria 2');
    expect(protocol('Hysteria 2').textContent).toMatch(/留空.*BBR/);
    expect(within(protocol('Hysteria 2')).getByText('hk.nodes.example.net', { exact: true })).toBeTruthy();
  });

  it('不允许给停用用户授权，并在入口改变授权范围时保留明确状态', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    const disabledUser = view.getByRole('button', { name: /legacy-user/ });
    expect(disabledUser.hasAttribute('disabled')).toBe(true);
    expect(disabledUser.getAttribute('title')).toContain('该用户已停用');
    fireEvent.click(view.getByRole('button', { name: /^alice(?:\s|$)/ }));
    expect(view.getByText('1 人已选')).toBeTruthy();
    expect(view.getByRole('button', { name: /^alice(?:\s|$)/ }).getAttribute('aria-pressed')).toBe('true');
  });

  it('拦截反向中转监听与任一入口协议撞端口', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '反向链' } });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), {
      target: { value: 'sg-relay-01' },
    });
    changeControl(view.getByRole('combobox', { name: '从 香港边缘 到 新加坡中转 的连接方式' }), {
      target: { value: 'reverse_v4' },
    });
    fireEvent.change(view.getByLabelText('香港边缘 的中转监听端口'), { target: { value: '14443' } });

    // 冲突收成端口旁的行内标记，页脚给出同一条原因。
    expect(view.getByRole('button', { name: /与 AnyTLS 接入口 TCP 14443 冲突/ })).toBeTruthy();
    expect(view.getByLabelText('香港边缘 的中转监听端口').getAttribute('aria-invalid')).toBe('true');
    expect(view.getAllByText(/与 AnyTLS 接入口 TCP 14443 冲突/)).toHaveLength(1);
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
  });

  it('路径上的机器与链路规则一样各自折叠，支路不提供移除', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), { target: { value: 'sg-relay-01' } });
    const heads = () => [...document.body.querySelectorAll<HTMLElement>('.chain-rule-node-head')];
    // 入口和刚加入的机器默认展开，两张规则表同时可见。
    expect(heads().map(head => head.getAttribute('aria-expanded'))).toEqual(['true', 'true']);
    expect(heads()[0].textContent).toContain('入口');
    expect(heads()[0].querySelector('.m-hop')?.textContent).toBe('—');
    // 只有入口有角色标签；中转、出口这类角色不再单独标出。
    expect([...heads()[1].querySelectorAll('.who > .st')]).toHaveLength(0);
    fireEvent.click(heads()[0]);
    expect(heads().map(head => head.getAttribute('aria-expanded'))).toEqual(['false', 'true']);
    expect(view.getByRole('button', { name: '把 新加坡中转 移出本链' })).toBeTruthy();
  });

  it('条件分流出去的支路由上游规则管理，行头没有移除', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), { target: { value: 'netflix' } });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), { target: { value: 'forward' } });
    const menu = openTargetMenu('香港边缘 第 1 条转发目标');
    fireEvent.click(within(menu).getByRole('button', { name: /NODE新加坡中转/ }));
    const branch = [...document.body.querySelectorAll<HTMLElement>('.chain-rule-node-head')].find(
      head => head.querySelector('.who > b')?.textContent === '新加坡中转',
    );
    expect(branch?.textContent).toContain('geosite=netflix');
    expect(branch?.getAttribute('aria-expanded')).toBe('true');
    expect(branch?.querySelector('.del-ctl')).toBeNull();
  });

  it('本机 DNS 解析策略在弹层里调整顺序，提交时写入同一批草稿', async () => {
    draft.init(`wizard-dns-order-${Math.random()}`);
    draft.clear();
    const client = queryClient();
    const snapshot = client.getQueryData<Record<string, unknown>>(['snapshot']);
    const resolution = {
      address: '1.1.1.1',
      port: 53,
      transport: 'udp',
      address_strategy: 'use_ip',
      fallback: 'machine',
    };
    client.setQueryData(['snapshot'], {
      ...snapshot,
      node_egress_dns: [
        { node: 'hk-edge-01', position: 0, selector: { t: 'geosite', v: ['netflix'] }, resolution },
        { node: 'hk-edge-01', position: 1, selector: { t: 'geosite', v: ['disney'] }, resolution },
      ],
    });
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: 'DNS 顺序' } });
    fireEvent.click(entryRules().getByRole('button', { name: '查看本机 DNS 解析策略' }));
    const dns = screen.getByRole('dialog', { name: '香港边缘 的本机 DNS 解析策略' });
    expect(
      within(dns)
        .getAllByRole('row')
        .map(row => row.textContent),
    ).toEqual([expect.stringContaining('netflix'), expect.stringContaining('disney')]);
    fireEvent.click(within(dns).getByRole('button', { name: 'DNS 优先级上移（geosite disney）' }));
    fireEvent.click(within(dns).getByRole('button', { name: '完成' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: /本机 DNS 解析策略/ })).toBeNull());
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    expect(draft.ops().find(op => op.op === 'reorder_node_egress_dns')).toEqual({
      op: 'reorder_node_egress_dns',
      node_id: 'hk-edge-01',
      selectors: [
        { t: 'geosite', v: ['disney'] },
        { t: 'geosite', v: ['netflix'] },
      ],
    });
  });

  it('域名规则提供嗅探失败兜底，传输层规则可选 TCP 或 UDP', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    expect(entryRules().queryByRole('button', { name: '＋ 嗅探失败兜底' })).toBeNull();
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 嗅探失败兜底' }));
    expect(
      (view.getAllByRole('combobox', { name: /香港边缘 第 \d+ 条匹配条件/ }) as HTMLSelectElement[]).map(
        select => select.value,
      ),
    ).toEqual(['geosite', 'sniffing_failed', 'any']);
    expect(entryRules().queryByRole('button', { name: '＋ 嗅探失败兜底' })).toBeNull();
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'network' },
    });
    const network = view.getByRole('combobox', { name: '香港边缘 第 1 条传输层' }) as HTMLSelectElement;
    expect(network.value).toBe('tcp');
    fireEvent.change(network, { target: { value: 'udp' } });
    expect((view.getByRole('combobox', { name: '香港边缘 第 1 条传输层' }) as HTMLSelectElement).value).toBe('udp');
  });

  it('自定义路径必须填写纯主机地址', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '自定义链' } });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), {
      target: { value: 'sg-relay-01' },
    });
    // 自定义地址就在拨号菜单的最后一项里填写，与链路规则一致。
    const dial = view.getByRole('combobox', { name: '从 香港边缘 到 新加坡中转 的连接方式' });
    fireEvent.click(dial);
    const address = within(screen.getByRole('listbox', { name: '从 香港边缘 到 新加坡中转 的连接方式' })).getByRole(
      'textbox',
      { name: '自定义地址' },
    );
    fireEvent.change(address, { target: { value: '10' } });
    fireEvent.change(address, { target: { value: '' } });
    expect(dial.textContent).toBe('自定义');
    expect(view.container.querySelector('.rule-dial-address.is-problem')?.textContent).toBe('在拨号菜单里填写地址');
    expect(view.getAllByText(/填写自定义主机地址/)).toHaveLength(1);
    fireEvent.change(address, { target: { value: '10.0.0.8' } });
    expect(view.container.querySelector('.rule-dial-address')?.textContent).toBe('10.0.0.8');
    expect(view.queryAllByText(/填写自定义主机地址/)).toHaveLength(0);
  });

  it('例外支路、主干、末尾节点地区和授权按预览写入同一批草稿', async () => {
    draft.init(`wizard-graph-${Math.random()}`);
    draft.clear();
    const client = queryClient();
    const third = {
      ...agentNodes[1],
      node_id: 'tw-exit-01',
      name: '台湾出口',
      public_ipv4: '192.0.2.30',
      public_ipv4_country: 'TW',
    };
    client.setQueryData(['nodes'], { nodes: [...agentNodes, third] });
    const snapshot = client.getQueryData<{ snapshot: { nodes: unknown[]; apps: unknown[] } }>(['snapshot']);
    client.setQueryData(['snapshot'], {
      ...snapshot,
      snapshot: {
        ...snapshot?.snapshot,
        nodes: [...(snapshot?.snapshot.nodes ?? []), { id: third.node_id, certificate_name: 'tw.nodes.example.net' }],
      },
    });
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '亚太分流' } });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), { target: { value: 'sg-relay-01' } });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'domain_suffix' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.com' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), {
      target: { value: 'forward' },
    });
    const menu = openTargetMenu('香港边缘 第 1 条转发目标');
    expect(menu.textContent).toContain('在机器上新建本链监听');
    expect(menu.textContent).toContain('外部出站');
    fireEvent.click(within(document.body).getByRole('button', { name: /NODE台湾出口.*加入本链/ }));
    expect(view.container.querySelector('.rule-dial-address')?.textContent).toBe('192.0.2.30');
    expect(view.queryByRole('combobox', { name: '订阅地区' })).toBeNull();
    expect(view.queryByLabelText('出口地区标识')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: /^alice(?:\s|$)/ }));
    const preview = view.getByRole('button', { name: '配置预览' });
    fireEvent.click(preview);
    expect(view.getByText(/香港边缘：domain_suffix=example.com → 转发 台湾出口；任意 → 转发 新加坡中转/)).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());

    const ops = draft.ops();
    expect(ops.map(op => op.op)).toEqual([
      'create_chain',
      'create_ingress',
      'put_step',
      'put_step',
      'put_step',
      'upsert_grant',
    ]);
    const steps = ops.filter(op => op.op === 'put_step');
    expect(steps.map(op => op.node_id)).toEqual(['hk-edge-01', 'tw-exit-01', 'sg-relay-01']);
    const entry = steps[0];
    if (entry.op !== 'put_step') throw new Error('入口 step 没有写入');
    expect(entry.step.rules).toMatchObject([
      { m: { t: 'domain_suffix', v: ['example.com'] }, a: { t: 'forward', to: 'tw-exit-01' } },
      { m: { t: 'any' }, a: { t: 'forward', to: 'sg-relay-01' } },
    ]);
    expect(steps[1]).toMatchObject({ step: { accept: {}, hop_in: { port: 20000 } } });
    expect(steps[2]).toMatchObject({ step: { accept: {}, hop_in: { port: 20000 } } });
    expect(ops[0]).toMatchObject({ chain: { subscription_country: 'SG' } });
    expect(ops.at(-1)).toMatchObject({ grant: { user_id: 'alice' } });
  });

  it('路径规则可引用已有 VPN Gate 出站，且与线路规则共用机器资格判定', async () => {
    draft.init(`wizard-vpngate-${Math.random()}`);
    draft.clear();
    const client = queryClient();
    client.setQueryData(['vpngate'], {
      countries: [
        {
          country_code: 'JP',
          country_name: 'Japan',
          current_servers: 1,
          retained_servers: 1,
          measured_successful: 1,
          candidate_servers: 1,
        },
      ],
      status: {},
      manual_pools_supported: true,
    });
    client.setQueryData(['nodes'], {
      nodes: [
        {
          ...agentNodes[0],
          agent_protocol_version: 20,
          lifecycle_phase: 'active',
          operationally_isolated: false,
          runtime_report_fresh: true,
          runtime_versions: { openvpn: '2.6.14' },
        },
        agentNodes[1],
      ],
    });
    const snapshot = client.getQueryData<{ snapshot: object }>(['snapshot']);
    client.setQueryData(['snapshot'], {
      ...snapshot,
      snapshot: {
        ...snapshot?.snapshot,
        external_outbounds: [
          {
            id: 'vpn-jp',
            tenant: 'platform',
            name: 'VPN Gate · 日本',
            address: 'managed.vpngate.invalid',
            port: 1,
            protocol: {
              t: 'vpngate',
              v: { country_code: 'JP', max_connect_ms: 15000, min_download_bps: 1000000, max_candidates: 16 },
            },
            security: { t: 'none' },
            bindings: [],
          },
        ],
      },
    });
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: 'VPN Gate 测试' } });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'domain_suffix' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.org' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), {
      target: { value: 'forward' },
    });
    openTargetMenu('香港边缘 第 1 条转发目标');
    const vpnOption = within(document.body).getByRole('button', { name: 'VPN Gate' });
    expect(vpnOption.hasAttribute('disabled')).toBe(false);
    expect(within(document.body).queryByRole('button', { name: /VGVPN Gate · 日本.*共享资源/ })).toBeNull();
    fireEvent.click(vpnOption);
    fireEvent.click(await within(document.body).findByRole('button', { name: '日本 自动节点池' }));
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    expect(draft.ops().find(op => op.op === 'put_step')).toMatchObject({
      step: { rules: [{ a: { t: 'proxy', outbound: 'vpn-jp' } }, { a: { t: 'egress' } }] },
    });
  });

  it('提交同时保留协议参数、授权、主路径和机器级 DNS，缺少 DNS 地址时阻止提交', async () => {
    draft.init(`wizard-dns-${Math.random()}`);
    draft.clear();
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard defaultApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: 'DNS 测试' } });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), {
      target: { value: 'sg-relay-01' },
    });
    fireEvent.click(view.getByRole('button', { name: /^alice(?:\s|$)/ }));
    const encryption = view.getByRole('checkbox', { name: 'VLESS · Encryption' });
    fireEvent.click(encryption);
    fireEvent.click(within(encryption.closest('.wzp-card') as HTMLElement).getByRole('button', { name: '参数' }));
    fireEvent.change(view.getByRole('combobox', { name: '握手档位' }), { target: { value: 'native' } });
    fireEvent.change(view.getByRole('textbox', { name: 'VLESS Encryption 监听端口' }), {
      target: { value: '25443' },
    });
    fireEvent.click(entryRules().getByRole('button', { name: '＋ 加一条' }));
    expect((view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }) as HTMLSelectElement).value).toBe(
      'geosite',
    );
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'netflix' },
    });
    changeControl(view.getByRole('combobox', { name: 'DNS 解析方式（新建链：香港边缘 第 1 条）' }), {
      target: { value: 'custom' },
    });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    // 选「自定义 DNS 解析」直接打开本机 DNS 解析策略弹层，停在这一条上。
    const dns = screen.getByRole('dialog', { name: '香港边缘 的本机 DNS 解析策略' });
    fireEvent.change(within(dns).getByRole('textbox', { name: /^DNS 地址/ }), {
      target: { value: '1.1.1.1' },
    });
    fireEvent.click(within(dns).getByRole('button', { name: '完成' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: /本机 DNS 解析策略/ })).toBeNull());
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    const ops = draft.ops();
    expect(ops.map(op => op.op)).toEqual([
      'create_chain',
      'create_ingress',
      'put_step',
      'put_step',
      'set_node_egress_dns',
      'upsert_grant',
    ]);
    expect(ops[0]).toMatchObject({ chain: { name: 'DNS 测试', subscription_country: 'SG' } });
    expect(ops.find(op => op.op === 'create_ingress')).toMatchObject({
      ingress: {
        node_id: 'hk-edge-01',
        reality: { fallback_mode: 'node-certificate' },
        wires: {
          vless: { kind: 'vless-reality' },
          vless_encryption: { port: 25443, options: { appearance: 'native', ticket_lifetime: '600s' } },
          anytls: { port: 14443 },
          hysteria2: { port: 30000, hop: { start: 30000, end: 30099 } },
        },
      },
    });
    expect(ops.find(op => op.op === 'put_step' && op.node_id === 'hk-edge-01')).toMatchObject({
      step: {
        rules: [
          { m: { t: 'geosite', v: ['netflix'] }, a: { t: 'egress' } },
          { m: { t: 'any' }, a: { t: 'forward', to: 'sg-relay-01' } },
        ],
      },
    });
    expect(ops.find(op => op.op === 'put_step' && op.node_id === 'sg-relay-01')).toMatchObject({
      step: { hop_in: { port: 20000 }, rules: [{ m: { t: 'any' }, a: { t: 'egress' } }] },
    });
    expect(ops.find(op => op.op === 'set_node_egress_dns')).toMatchObject({
      node_id: 'hk-edge-01',
      selector: { t: 'geosite', v: ['netflix'] },
      resolution: { address: '1.1.1.1', port: 53 },
    });
    expect(ops.at(-1)).toMatchObject({ grant: { tenant_id: 'platform', user_id: 'alice' } });
  });
});

describe('纳管自动初始化', () => {
  it.each([
    ['queued', '等待自动初始化', '等待自动初始化…'],
    ['running', 'Agent 已上线，初始化中', '查看初始化发布单'],
    ['succeeded', '初始化完成，可创建线路', '查看机器'],
    ['manual', '需要人工发布', '查看发布计划'],
    ['failed', '初始化需要处理', '查看失败发布单'],
  ] as const)('根据服务端 %s 状态展示初始化进度，不自行创建发布单', (state, title, action) => {
    const client = queryClient();
    client.setQueryData(['nodes'], {
      nodes: [
        {
          ...agentNodes[0],
          token_last_used_at: new Date().toISOString(),
          last_poll_at: new Date().toISOString(),
          initialization: {
            state,
            revision_id: 13,
            deployment_id: state === 'running' || state === 'failed' ? 21 : null,
            reason: state === 'manual' ? '已有其他机器的配置待发布' : null,
          },
        },
      ],
    });
    const fetchMock = vi.fn(async (_url?: unknown, _init?: RequestInit) =>
      Response.json({
        hosts: [],
        instances: [],
        quotas: [],
        can_manage_quotas: false,
        can_write: false,
        redacted: false,
      }),
    );
    vi.stubGlobal('fetch', fetchMock);
    const go = vi.fn();
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ProvisionInstall node="hk-edge-01" result={installResult()} go={go} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    expect(view.getByText(title)).toBeTruthy();
    const button = view.getByRole('button', { name: action });
    expect(button.hasAttribute('disabled')).toBe(state === 'queued');
    expect(view.queryByRole('button', { name: /去发布 · 计划预览/ })).toBeNull();
    if (state === 'succeeded') {
      fireEvent.click(button);
      expect(go).toHaveBeenCalledWith({ p: 'node', id: 'hk-edge-01' });
      expect(within(view.getByRole('list', { name: '纳管进度' })).getAllByRole('listitem')[2].className).toBe('done');
    }
    if (state === 'manual') expect(view.getByText('已有其他机器的配置待发布')).toBeTruthy();
    expect(
      fetchMock.mock.calls.every(([url, init]) => url === '/cvm' && (!init?.method || init.method === 'GET')),
    ).toBe(true);
  });

  it('WireGuard 初始关闭，允许操作者显式开启', () => {
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <ProvisionForm go={() => undefined} />
      </QueryClientProvider>,
    );
    const toggle = view.getByRole('group', { name: 'WireGuard overlay' });
    expect(within(toggle).getByRole('button', { name: '关闭' }).getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(within(toggle).getByRole('button', { name: '启用' }));
    expect(within(toggle).getByRole('button', { name: '启用' }).getAttribute('aria-pressed')).toBe('true');
  });
});
