import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { ExternalOutbound } from '../src/api';
import { draft } from '../src/draft';
import {
  WarpBindingCard,
  WarpEdit,
  WarpRuleManager,
  warpIpRouting,
  warpIpStackOf,
  warpReferenceStatus,
} from '../src/panes/tunnels';

type WarpProtocol = Extract<ExternalOutbound['protocol'], { t: 'warp' }>;

const protocol = (overrides: Partial<WarpProtocol['v']> = {}): WarpProtocol => ({
  t: 'warp',
  v: {
    mtu: 1280,
    keep_alive: 25,
    allowed_ips: ['0.0.0.0/0', '::/0'],
    no_kernel_tun: false,
    domain_strategy: 'ForceIP',
    workers: 0,
    ...overrides,
  },
});

describe('WARP exit stack settings', () => {
  it('does not label an unused tunnel as unreferenced', () => {
    expect(warpReferenceStatus(0, 0)).toBeNull();
    expect(warpReferenceStatus(2, 1)).toBe('待注册');
    expect(warpReferenceStatus(2, 0)).toBe('已引用');
  });

  it('maps every operator choice to matching Xray routing knobs', () => {
    expect(warpIpRouting('ipv4')).toEqual({
      allowed_ips: ['0.0.0.0/0'],
      domain_strategy: 'ForceIPv4',
    });
    expect(warpIpRouting('dual')).toEqual({
      allowed_ips: ['0.0.0.0/0', '::/0'],
      domain_strategy: 'ForceIP',
    });
    expect(warpIpRouting('prefer_ipv4')).toEqual({
      allowed_ips: ['0.0.0.0/0', '::/0'],
      domain_strategy: 'ForceIPv4v6',
    });
    expect(warpIpRouting('prefer_ipv6')).toEqual({
      allowed_ips: ['0.0.0.0/0', '::/0'],
      domain_strategy: 'ForceIPv6v4',
    });
    expect(warpIpRouting('ipv6')).toEqual({
      allowed_ips: ['::/0'],
      domain_strategy: 'ForceIPv6',
    });
  });

  it('does not widen legacy or custom IPv4-only tunnels when they are edited', () => {
    expect(warpIpStackOf(protocol())).toBe('dual');
    expect(warpIpStackOf(protocol({ domain_strategy: 'ForceIPv4' }))).toBe('ipv4');
    expect(warpIpStackOf(protocol({ domain_strategy: 'ForceIPv4v6' }))).toBe('prefer_ipv4');
    expect(warpIpStackOf(protocol({ domain_strategy: 'ForceIPv6v4' }))).toBe('prefer_ipv6');
    expect(warpIpStackOf(protocol({ allowed_ips: ['0.0.0.0/0'] }))).toBe('ipv4');
    expect(warpIpStackOf(protocol({ allowed_ips: ['::/0'] }))).toBe('ipv6');
  });

  it('makes the stack and Keepalive editable after the tunnel is created', async () => {
    draft.clear();
    const tunnel: ExternalOutbound = {
      id: 'warp',
      tenant: 'platform',
      name: 'Cloudflare WARP',
      address: 'engage.cloudflareclient.com',
      port: 2408,
      protocol: protocol(),
      security: { t: 'none' },
      bindings: [],
    };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const onClose = vi.fn();
    const view = render(
      <QueryClientProvider client={client}>
        <WarpEdit tunnel={tunnel} onClose={onClose} />
      </QueryClientProvider>,
    );

    const keepAlive = view.getByRole('spinbutton', { name: 'Keepalive' });
    expect((keepAlive as HTMLInputElement).value).toBe('25');
    fireEvent.change(keepAlive, { target: { value: '40' } });
    expect((keepAlive as HTMLInputElement).value).toBe('40');

    const stack = view.getByRole('combobox', { name: 'WARP 出口协议栈' });
    fireEvent.change(stack, { target: { value: 'ipv4' } });
    expect((stack as HTMLSelectElement).value).toBe('ipv4');

    const tun = view.getByRole('combobox', { name: 'TUN 实现' });
    fireEvent.change(tun, { target: { value: 'userspace' } });
    const workers = view.getByRole('spinbutton', { name: 'Workers' });
    fireEvent.change(workers, { target: { value: '4' } });

    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    const operation = draft.ops()[0];
    expect(operation?.op).toBe('upsert_external_outbound');
    if (operation?.op !== 'upsert_external_outbound' || operation.outbound.protocol.t !== 'warp') {
      throw new Error('expected a WARP draft operation');
    }
    expect(operation.outbound.protocol.v.keep_alive).toBe(40);
    expect(operation.outbound.protocol.v.allowed_ips).toEqual(['0.0.0.0/0']);
    expect(operation.outbound.protocol.v.domain_strategy).toBe('ForceIPv4');
    expect(operation.outbound.protocol.v.no_kernel_tun).toBe(true);
    expect(operation.outbound.protocol.v.workers).toBe(4);
    draft.clear();
  });

  it('sends every machine-level setting as an independent override', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ revision_id: 2, binding: {} }),
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <WarpBindingCard
          tenantId="platform"
          outboundId="warp"
          binding={{
            node: 'hk',
            device_id: 'device-hk',
            account_id: 'account-hk',
            registered_at: '2026-08-28T00:00:00.000Z',
            peer_public_key: 'peer-key',
            local_addresses: ['172.16.0.2/32', '2606:4700::2/128'],
            reserved: [1, 2, 3],
          }}
          nodeName="香港"
          defaultAddress="engage.cloudflareclient.com"
          defaultPort={2408}
          defaults={protocol()}
          editable
        />
      </QueryClientProvider>,
    );
    const card = within(view.container);

    fireEvent.click(card.getByRole('button', { name: '设置' }));
    fireEvent.change(card.getByLabelText('Keepalive（秒）'), { target: { value: '35' } });
    fireEvent.change(card.getByRole('combobox', { name: 'WARP 出口协议栈' }), {
      target: { value: 'prefer_ipv6' },
    });
    fireEvent.change(card.getByLabelText('TUN 实现'), { target: { value: 'userspace' } });
    fireEvent.change(card.getByLabelText('Workers'), { target: { value: '8' } });
    fireEvent.click(card.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({
      endpoint_address: null,
      endpoint_port: null,
      mtu: null,
      keep_alive: 35,
      allowed_ips: ['0.0.0.0/0', '::/0'],
      domain_strategy: 'ForceIPv6v4',
      no_kernel_tun: true,
      workers: 8,
    });
    vi.unstubAllGlobals();
  });

  it('requires an inline confirmation before unregistering and removing a machine identity', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ revision_id: 3, node_id: 'hk', device_id: 'device-hk', removed: true }),
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <WarpBindingCard
          tenantId="platform"
          outboundId="warp"
          binding={{
            node: 'hk',
            device_id: 'device-hk',
            account_id: 'account-hk',
            registered_at: '2026-08-28T00:00:00.000Z',
            peer_public_key: 'peer-key',
            local_addresses: ['172.16.0.2/32', '2606:4700::2/128'],
            reserved: [1, 2, 3],
          }}
          nodeName="香港"
          defaultAddress="engage.cloudflareclient.com"
          defaultPort={2408}
          defaults={protocol()}
          editable
        />
      </QueryClientProvider>,
    );
    const card = within(view.container);

    fireEvent.click(card.getByRole('button', { name: '设置' }));
    fireEvent.click(card.getByRole('button', { name: '注销并移除' }));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(card.getByText('注销这台机器的 Cloudflare 身份？')).toBeTruthy();
    expect(card.getByText(/历史修订也不能恢复这个身份/)).toBeTruthy();

    fireEvent.click(card.getByRole('button', { name: '确认注销并移除' }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('/tenants/platform/tunnels/warp/warp-bindings/hk');
    expect(init.method).toBe('DELETE');
    expect(init.body).toBeUndefined();
    vi.unstubAllGlobals();
  });

  it('explains why a referenced machine identity cannot be removed', () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <WarpBindingCard
          tenantId="platform"
          outboundId="warp"
          binding={{
            node: 'hk',
            device_id: 'device-hk',
            account_id: 'account-hk',
            registered_at: '2026-08-28T00:00:00.000Z',
            peer_public_key: 'peer-key',
            local_addresses: ['172.16.0.2/32'],
            reserved: [1, 2, 3],
          }}
          nodeName="香港"
          defaultAddress="engage.cloudflareclient.com"
          defaultPort={2408}
          defaults={protocol()}
          editable
          removalBlockedReason="这台机器仍被规则引用。请先解除引用并完成发布，再注销身份。"
        />
      </QueryClientProvider>,
    );
    const card = within(view.container);

    fireEvent.click(card.getByRole('button', { name: '设置' }));
    const remove = card.getByRole('button', { name: '注销并移除' }) as HTMLButtonElement;
    expect(remove.disabled).toBe(true);
    expect(card.getByText(/请先解除引用并完成发布/)).toBeTruthy();
  });

  it('registers the current rule machine only after explicit terms acceptance', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        revision_id: 4,
        binding: {},
        suggested_endpoint: 'engage.cloudflareclient.com:2408',
      }),
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const onClose = vi.fn();
    const tunnel: ExternalOutbound = {
      id: 'warp',
      tenant: 'platform',
      name: 'Cloudflare WARP',
      address: 'engage.cloudflareclient.com',
      port: 2408,
      protocol: protocol(),
      security: { t: 'none' },
      bindings: [],
    };
    const view = render(
      <QueryClientProvider client={client}>
        <WarpRuleManager tunnel={tunnel} nodeId="hk" nodeName="香港" editable onClose={onClose} />
      </QueryClientProvider>,
    );

    const register = view.getByRole('button', { name: '注册并绑定当前机器' }) as HTMLButtonElement;
    expect(register.disabled).toBe(true);
    fireEvent.click(view.getByRole('checkbox'));
    expect(register.disabled).toBe(false);
    fireEvent.click(register);

    await waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('/tenants/platform/tunnels/warp/warp-bindings');
    expect(init.method).toBe('POST');
    expect(JSON.parse(String(init.body))).toEqual({ node_id: 'hk', accept_terms: true });
    expect(onClose).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});
