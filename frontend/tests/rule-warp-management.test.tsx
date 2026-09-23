import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  AGENT_PROTOCOL_VERSION,
  fetchSnapshot,
  registerWarpBinding,
  type ConsoleSnapshot,
  type ExternalOutbound,
  type NodeAgentStateItem,
  type Rule,
} from '../src/api';
import { draft } from '../src/draft';
import { ExternalOutboundEditor, RuleEditor } from '../src/panes/rules';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addListener: () => undefined,
  removeListener: () => undefined,
  addEventListener: () => undefined,
  removeEventListener: () => undefined,
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const warp: ExternalOutbound = {
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
};

const vendor: ExternalOutbound = {
  id: 'custom-1111-1111',
  tenant: 'platform',
  name: '供应商出口',
  address: 'edge.example.com',
  port: 443,
  protocol: {
    t: 'vless',
    v: { credential: '<redacted>', encryption: 'none', flow: null, transport: { t: 'raw' } },
  },
  security: { t: 'tls', v: { server_name: 'edge.example.com', fingerprint: 'chrome' } },
  bindings: [],
};

const vpngate: ExternalOutbound = {
  id: 'vpngate-1111-1111',
  tenant: 'platform',
  name: 'VPN Gate 日本',
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
};

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
  window.history.replaceState(null, '', '#/nodes');
});

function snapshotWith(outbounds: ExternalOutbound[] = [warp, vendor], chainTenant = 'platform'): ConsoleSnapshot {
  return {
    snapshot: {
      revision: 1,
      apps: [
        {
          id: 'video',
          label: '视频',
          chains: [{ id: 'stream', tenant: chainTenant, name: '流媒体' }],
          steps: [],
          ingresses: [],
          fronts: [],
          grants: [],
        },
      ],
      external_outbounds: outbounds,
    },
    node_egress_dns: [],
    redacted: false,
  };
}

function renderEditor(
  initial: Rule[],
  committedOutbounds: ExternalOutbound[] = [warp, vendor],
  chainTenant = 'platform',
  displayedOutbounds: ExternalOutbound[] = [warp, vendor],
  nodePatch: Partial<NodeAgentStateItem> = {},
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  const snapshot = snapshotWith(displayedOutbounds, chainTenant);
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['snapshot', 'committed'], snapshotWith(committedOutbounds, chainTenant));
  client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
  client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
  client.setQueryData(['nodes'], {
    nodes: [
      {
        node_id: 'hk',
        tenant_id: 'platform',
        name: '香港落地',
        public_ipv4: 'hk.example.net',
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        egress_allowed: true,
        ...nodePatch,
      } as NodeAgentStateItem,
    ],
  });

  return render(
    <QueryClientProvider client={client}>
      <RuleEditor
        appId="video"
        chainId="stream"
        nodeId="hk"
        initial={initial}
        accept={null}
        peers={[]}
        isForwardTarget={false}
        fallback={{ rules: [], pending: false }}
      />
    </QueryClientProvider>,
  );
}

describe('tunnel navigation from forwarding rules', () => {
  it('shows VPN Gate but does not offer it on a node without OpenVPN', () => {
    const view = renderEditor(
      [{ m: { t: 'any' }, a: { t: 'proxy', outbound: vendor.id } }],
      [warp, vendor, vpngate],
      'platform',
      [warp, vendor, vpngate],
      {
        lifecycle_phase: 'active',
        operationally_isolated: false,
        agent_protocol_version: AGENT_PROTOCOL_VERSION,
        runtime_report_fresh: true,
        runtime_versions: {
          agent: 'agent-build',
          xray: null,
          phantun: null,
          openvpn: null,
          wg_tools: null,
          wg_backend: null,
        },
      },
    );

    fireEvent.click(view.getByRole('button', { name: /供应商出口/ }));
    const choice = view.getByRole('button', { name: 'VPN Gate' });
    expect(choice?.hasAttribute('disabled')).toBe(true);
    expect(view.getByText(/不可接入 · 未安装或无法执行 OpenVPN/)).toBeTruthy();
  });

  it('warns when an existing VPN Gate rule is viewed after OpenVPN disappears', () => {
    const view = renderEditor(
      [{ m: { t: 'any' }, a: { t: 'proxy', outbound: vpngate.id } }],
      [warp, vendor, vpngate],
      'platform',
      [warp, vendor, vpngate],
      {
        lifecycle_phase: 'active',
        operationally_isolated: false,
        agent_protocol_version: AGENT_PROTOCOL_VERSION,
        runtime_report_fresh: true,
        runtime_versions: {
          agent: 'agent-build',
          xray: null,
          phantun: null,
          openvpn: null,
          wg_tools: null,
          wg_backend: null,
        },
      },
    );

    expect(view.getByRole('alert').textContent).toContain('当前机器不纳入 VPN Gate 接入节点');
    expect(view.getByRole('alert').textContent).toContain('发布会被控制面阻止');
  });

  it('opens an unselected outbound in the tunnel page', () => {
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: vendor.id } }]);

    fireEvent.click(view.getByRole('button', { name: /供应商出口/ }));
    fireEvent.click(view.getByRole('button', { name: '打开隧道 Cloudflare WARP' }));

    expect(window.location.hash).toBe('#/tunnels/warp/warp-8f3a-2d71');
  });

  it('opens the selected outbound summary in the tunnel page', () => {
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: warp.id } }]);

    fireEvent.click(view.getByRole('button', { name: '隧道详情' }));

    expect(window.location.hash).toBe('#/tunnels/warp/warp-8f3a-2d71');
  });

  it('sends resource creation and editing to the tunnel page', () => {
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: warp.id } }]);

    fireEvent.click(view.getByRole('button', { name: /Cloudflare WARP/ }));
    expect(view.queryByRole('button', { name: '删除 供应商出口' })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '管理隧道' }));

    expect(window.location.hash).toBe('#/tunnels');
  });

  it('lets the operator finish the current machine WARP identity without leaving the rule', async () => {
    const committed = snapshotWith();
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      void init;
      const url = String(input);
      const body = url.endsWith('/warp-bindings')
        ? {
            revision_id: 2,
            binding: {
              node: 'hk',
              device_id: 'device-hk',
              account_id: 'account-hk',
              registered_at: '2026-09-12T00:00:00Z',
              peer_public_key: 'peer-key',
              local_addresses: ['172.16.0.2/32'],
              reserved: [1, 2, 3],
            },
          }
        : url === '/model/snapshot'
          ? committed
          : url === '/revisions?limit=50'
            ? { current_revision: null, revisions: [] }
            : (() => {
                throw new Error(`unexpected request ${url}`);
              })();
      return { ok: true, status: 200, json: async () => body } as Response;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: warp.id } }]);

    expect(view.getByText('香港落地 尚无 WARP 身份')).toBeTruthy();
    const register = view.getByRole('button', { name: '为 香港落地 申请 WARP 身份' }) as HTMLButtonElement;
    expect(register.disabled).toBe(true);
    fireEvent.click(view.getByRole('checkbox', { name: /Cloudflare Application Terms/ }));
    expect(register.disabled).toBe(false);
    fireEvent.click(register);

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        `/tenants/platform/tunnels/${warp.id}/warp-bindings`,
        expect.objectContaining({ method: 'POST' }),
      ),
    );
    const call = fetchMock.mock.calls.find(([input]) => String(input).endsWith('/warp-bindings'));
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ node_id: 'hk', accept_terms: true });
    await waitFor(() => expect(view.getByText('香港落地 的 WARP 身份已就绪')).toBeTruthy());
  });

  it('does not let an unrelated browser draft block WARP registration', () => {
    draft.push({
      op: 'put_step',
      app_id: 'another-app',
      chain_id: 'another-chain',
      node_id: 'hk',
      step: { rules: [] },
    });
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: warp.id } }]);

    fireEvent.click(view.getByRole('checkbox', { name: /Cloudflare Application Terms/ }));
    expect((view.getByRole('button', { name: '为 香港落地 申请 WARP 身份' }) as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  it('explains why a draft-only WARP cannot create an external identity yet', () => {
    const view = renderEditor([{ m: { t: 'any' }, a: { t: 'proxy', outbound: warp.id } }], [vendor]);

    expect(view.getByText('这条 WARP 还只存在于当前变更集。先创建修订，再回来申请机器身份。')).toBeTruthy();
    expect(view.queryByRole('checkbox', { name: /Cloudflare Application Terms/ })).toBeNull();
  });

  it('does not offer an ancestor WARP that cannot bind a child-tenant machine', () => {
    const view = renderEditor(
      [{ m: { t: 'any' }, a: { t: 'proxy', outbound: vendor.id } }],
      [warp, vendor],
      'platform.child',
    );

    fireEvent.click(view.getByRole('button', { name: /供应商出口/ }));
    expect(view.queryByRole('button', { name: '打开隧道 Cloudflare WARP' })).toBeNull();
    expect(view.getByRole('button', { name: '打开隧道 供应商出口' })).toBeTruthy();
  });
});

describe('external outbound editor controls', () => {
  it('gives grouped tunnel controls independent accessible names', () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <ExternalOutboundEditor
          tenantId="platform"
          existing={null}
          onClose={() => undefined}
          onSaved={() => undefined}
        />
      </QueryClientProvider>,
    );

    fireEvent.click(view.getByRole('button', { name: '手动填写' }));
    expect(view.getByRole('button', { name: 'RAW / TCP' })).toBeTruthy();
    expect(view.getByRole('button', { name: 'XHTTP' })).toBeTruthy();
    expect(view.getByRole('textbox', { name: '服务器地址' })).toBeTruthy();
    expect(view.getByRole('spinbutton', { name: '服务器端口' })).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: 'XHTTP' }));
    expect(view.getByRole('combobox', { name: 'XHTTP 上传模式' })).toBeTruthy();
    expect(view.getByRole('spinbutton', { name: 'XHTTP 上传 XMUX' })).toBeTruthy();
    fireEvent.click(view.getByRole('checkbox', { name: /下载使用另一组服务器/ }));
    expect(view.getByRole('textbox', { name: '下载服务器地址' })).toBeTruthy();
    expect(view.getByRole('spinbutton', { name: '下载服务器端口' })).toBeTruthy();
    expect(view.getByRole('combobox', { name: 'XHTTP 下载模式' })).toBeTruthy();
    expect(view.getByRole('spinbutton', { name: 'XHTTP 下载 XMUX' })).toBeTruthy();
    expect(view.getByRole('textbox', { name: '下载 SNI' })).toBeTruthy();
    expect(view.getByRole('textbox', { name: '下载 TLS 指纹' })).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: 'WireGuard' }));
    expect(view.getByRole('spinbutton', { name: 'WireGuard MTU' })).toBeTruthy();
    expect(view.getByRole('spinbutton', { name: 'WireGuard Keepalive' })).toBeTruthy();
  });

  it('does not create a draft operation when an existing outbound has no changes', () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <ExternalOutboundEditor
          tenantId="platform"
          existing={vendor}
          onClose={() => undefined}
          onSaved={() => undefined}
        />
      </QueryClientProvider>,
    );

    const save = view.getByRole('button', { name: '保存并选中' }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    expect(save.title).toBe('没有修改');
    fireEvent.click(save);
    expect(draft.ops()).toHaveLength(0);

    fireEvent.change(view.getByRole('textbox', { name: '名称' }), { target: { value: '供应商出口 2' } });
    expect(save.disabled).toBe(false);
  });
});

describe('WARP writes with an active draft', () => {
  it('drops the old draft preview cache after registration changes machine identity', async () => {
    let previewReads = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === '/model/preview') {
          previewReads += 1;
          return new Response(JSON.stringify({ snapshot: { preview: previewReads } }), { status: 200 });
        }
        if (url.endsWith('/warp-bindings')) {
          return new Response(
            JSON.stringify({
              revision_id: 2,
              binding: {
                node: 'hk',
                device_id: 'device-hk',
                account_id: 'account-hk',
                registered_at: '2026-09-12T00:00:00Z',
                peer_public_key: 'peer-key',
                local_addresses: ['172.16.0.2/32'],
                reserved: [1, 2, 3],
              },
            }),
            { status: 200 },
          );
        }
        throw new Error(`unexpected request ${url}`);
      }),
    );
    draft.push({
      op: 'put_step',
      app_id: 'another-app',
      chain_id: 'another-chain',
      node_id: 'hk',
      step: { rules: [] },
    });

    expect(await fetchSnapshot()).toEqual({ preview: 1 });
    await registerWarpBinding('platform', 'warp', 'hk');
    expect(await fetchSnapshot()).toEqual({ preview: 2 });
    expect(previewReads).toBe(2);
  });
});
