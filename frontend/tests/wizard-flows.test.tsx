import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ChainWizard } from '../src/panes/chain-wizard';
import { draft } from '../src/draft';
import { ProvisionForm, ProvisionInstall } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { ProvisionNodeResult } from '../src/api';
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
    public_ipv6: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    retired_at: null,
  },
];

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
    expect(view.getByText('当前账号没有可用于纳管机器的租户')).toBeTruthy();
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
  it('没有机器时仍保留纸面与配置卡，并说明如何解除阻塞', () => {
    const client = queryClient();
    client.setQueryData(['nodes'], { nodes: [] });
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={() => undefined} />
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
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );
  }

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
    expect(view.queryByLabelText('新链摘要')).toBeNull();
    expect(view.container.querySelectorAll('.nd-paper')).toHaveLength(1);
    expect(view.container.querySelectorAll('.wz-head, .wz-foot')).toHaveLength(0);
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '亚太分流' } });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(false);
  });

  it('接入协议沿用详情页的逐行选择样式，图标与端口编辑不干扰开关', () => {
    const view = mount();
    const cards = [...view.container.querySelectorAll('.wzp-card')];
    expect(cards).toHaveLength(4);
    expect(cards.every(card => card.classList.contains('protocol-choice'))).toBe(true);
    expect(cards.every(card => card.querySelector('.protocol-choice-copy .protocol-choice-icon'))).toBe(true);
    expect(cards[0].querySelector('.protocol-choice-icon polygon')).toBeTruthy();
    expect(cards[3].querySelector('.protocol-choice-icon polygon[fill="#ffbc00"]')).toBeTruthy();

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

  it('选择用户不依赖入口节点，之后选择入口仍保留授权选择', () => {
    const view = mount();
    expect(view.queryByText('先选择入口节点，再添加可用用户。')).toBeNull();
    fireEvent.change(view.getByRole('combobox', { name: '添加可用用户' }), {
      target: { value: 'platform/alice' },
    });
    expect(view.getByRole('button', { name: '撤销 alice 的授权' })).toBeTruthy();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    expect(view.getByRole('button', { name: '撤销 alice 的授权' })).toBeTruthy();
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
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={() => undefined} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '添加可用用户' }), {
      target: { value: 'platform/alice' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'branch-edge-01' },
    });
    expect(view.getByRole('button', { name: '撤销 alice 的授权' })).toBeTruthy();
    expect(view.getByText('alice · 该用户不在当前入口的可授权范围内')).toBeTruthy();
    expect(view.getByText('alice 当前不可授权，本次不会写入。')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '换一台当入口' }));
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    expect(view.getByRole('button', { name: '撤销 alice 的授权' })).toBeTruthy();
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
              v: {
                mtu: 1280,
                keep_alive: 25,
                allowed_ips: ['0.0.0.0/0', '::/0'],
                no_kernel_tun: false,
                domain_strategy: 'ForceIP',
                workers: 0,
              },
            },
            security: { t: 'none' },
            bindings: [],
          },
        ],
      },
    });
    const view = mount(client);
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.click(view.getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), {
      target: { value: 'forward' },
    });
    fireEvent.click(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }));
    const menu = document.body.querySelector('.external-target-menu');
    expect([...menu!.querySelectorAll('.external-target-menu-label')].map(item => item.textContent)).toEqual([
      '本链已有监听节点',
      '在机器上新建本链监听',
      '代理出站',
    ]);
    expect(within(menu as HTMLElement).getByPlaceholderText('搜索机器或代理出站')).toBeTruthy();
    const warp = within(menu as HTMLElement).getByRole('button', { name: /WARPCloudflare WARP共享资源/ });
    expect(warp.classList.contains('external-target-option-select')).toBe(true);
    expect(warp.parentElement?.classList.contains('external-target-option')).toBe(true);
    expect(within(menu as HTMLElement).getByRole('button', { name: '打开隧道 Cloudflare WARP' })).toBeTruthy();
    expect(within(menu as HTMLElement).getByRole('button', { name: '管理隧道' })).toBeTruthy();
    fireEvent.click(within(menu as HTMLElement).getByRole('button', { name: /自定义.*复用已有监听/ }));
    expect(within(menu as HTMLElement).getByText('自定义 · 复用已有监听')).toBeTruthy();
    fireEvent.click(within(menu as HTMLElement).getByRole('button', { name: '返回目标列表' }));
    expect(within(menu as HTMLElement).getByPlaceholderText('搜索机器或代理出站')).toBeTruthy();
  });

  it('自定义子菜单复用已有监听并把引用写入新链规则', async () => {
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
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '复用监听测试' } });
    fireEvent.click(view.getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'domain_suffix' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.com' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), { target: { value: 'forward' } });
    fireEvent.click(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }));
    fireEvent.click(within(document.body).getByRole('button', { name: /自定义.*复用已有监听/ }));
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
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={done} />
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
        wires: {
          anytls: { padding_scheme: ['stop=4', '0=20-30'] },
          hysteria2: { bandwidth: { up: '200 mbps', down: '500 mbps' } },
        },
      },
    });
  });

  it('不允许给停用用户授权，并在入口改变授权范围时保留明确状态', () => {
    const view = mount();
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), {
      target: { value: 'hk-edge-01' },
    });
    const disabledUser = view.getByRole('button', { name: /legacy-user/ });
    expect(disabledUser.hasAttribute('disabled')).toBe(true);
    expect(disabledUser.getAttribute('title')).toBe('该用户已停用');
    fireEvent.change(view.getByRole('combobox', { name: '添加可用用户' }), {
      target: { value: 'platform/alice' },
    });
    expect(view.getByText('1 人已选')).toBeTruthy();
    expect(view.getByRole('button', { name: '撤销 alice 的授权' })).toBeTruthy();
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
    fireEvent.change(view.getByRole('combobox', { name: '从 香港边缘 到 新加坡中转 的连接方式' }), {
      target: { value: 'reverse_v4' },
    });
    fireEvent.change(view.getByLabelText('香港边缘 的中转监听端口'), { target: { value: '14443' } });

    expect(view.getAllByText(/与 AnyTLS 接入口 TCP 14443 冲突/)).toHaveLength(2);
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
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
    fireEvent.change(view.getByRole('combobox', { name: '从 香港边缘 到 新加坡中转 的连接方式' }), {
      target: { value: 'custom' },
    });

    expect(view.getAllByText(/填写自定义主机地址/)).toHaveLength(2);
    expect(view.getByLabelText('到 新加坡中转 的自定义主机地址').getAttribute('aria-invalid')).toBe('true');
    fireEvent.change(view.getByLabelText('到 新加坡中转 的自定义主机地址'), {
      target: { value: '10.0.0.8' },
    });
    expect(view.queryAllByText(/填写自定义主机地址/)).toHaveLength(0);
  });

  it('例外支路、主干、订阅地区和授权按预览写入同一批草稿', async () => {
    draft.init(`wizard-graph-${Math.random()}`);
    draft.clear();
    const client = queryClient();
    const third = {
      ...agentNodes[1],
      node_id: 'tw-exit-01',
      name: '台湾出口',
      public_ipv4: '192.0.2.30',
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
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: '亚太分流' } });
    fireEvent.change(view.getByRole('combobox', { name: '在路径末尾添加机器' }), { target: { value: 'sg-relay-01' } });
    fireEvent.click(view.getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'domain_suffix' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.com' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), {
      target: { value: 'forward' },
    });
    fireEvent.click(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }));
    const menu = document.body.querySelector('.external-target-menu');
    expect(menu?.textContent).toContain('在机器上新建本链监听');
    expect(menu?.textContent).toContain('代理出站');
    expect(menu?.textContent).toContain('管理隧道');
    fireEvent.click(within(document.body).getByRole('button', { name: /NODE台湾出口.*加入本链/ }));
    expect(view.container.querySelector('.wzr-dial-address')?.textContent).toBe('192.0.2.30');
    fireEvent.change(view.getByRole('combobox', { name: '订阅地区' }), { target: { value: 'JP' } });
    fireEvent.change(view.getByRole('combobox', { name: '添加可用用户' }), { target: { value: 'platform/alice' } });
    const preview = view.getByRole('button', { name: /预览 6 条草稿操作/ });
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
    expect(ops[0]).toMatchObject({ chain: { subscription_country: 'JP' } });
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
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: 'VPN Gate 测试' } });
    fireEvent.click(view.getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条匹配条件' }), {
      target: { value: 'domain_suffix' },
    });
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.org' },
    });
    fireEvent.change(view.getByRole('combobox', { name: '香港边缘 第 1 条动作' }), {
      target: { value: 'forward' },
    });
    fireEvent.click(view.getByRole('button', { name: '香港边缘 第 1 条转发目标' }));
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

  it('出网规则的自定义 DNS 使用机器级草稿操作，并在缺少地址时阻止提交', async () => {
    draft.init(`wizard-dns-${Math.random()}`);
    draft.clear();
    const done = vi.fn();
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <SessionProvider value={{ who: operator, initial }}>
          <ChainWizard fixedApp={{ id: 'global', label: '全球加速' }} onDone={done} />
        </SessionProvider>
      </QueryClientProvider>,
    );
    fireEvent.change(view.getByRole('combobox', { name: '选择入口节点' }), { target: { value: 'hk-edge-01' } });
    fireEvent.change(view.getByRole('textbox', { name: '链名称' }), { target: { value: 'DNS 测试' } });
    fireEvent.click(view.getByRole('button', { name: '＋ 加一条' }));
    fireEvent.change(view.getByRole('textbox', { name: '香港边缘 第 1 条匹配取值' }), {
      target: { value: 'example.com' },
    });
    fireEvent.change(view.getByRole('combobox', { name: /DNS 解析方式.*第 1 条/ }), {
      target: { value: 'custom' },
    });
    expect(view.getByRole('button', { name: '加入草稿' }).hasAttribute('disabled')).toBe(true);
    fireEvent.change(view.getByRole('textbox', { name: /DNS 地址.*第 1 条/ }), {
      target: { value: '1.1.1.1' },
    });
    fireEvent.click(view.getByRole('button', { name: '加入草稿' }));
    await waitFor(() => expect(done).toHaveBeenCalledOnce());
    expect(draft.ops().find(op => op.op === 'set_node_egress_dns')).toMatchObject({
      node_id: 'hk-edge-01',
      selector: { t: 'domain_suffix', v: ['example.com'] },
      resolution: { address: '1.1.1.1', port: 53 },
    });
  });
});
