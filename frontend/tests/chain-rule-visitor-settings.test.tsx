import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { SnapshotChain, SnapshotIngress, SnapshotStep } from '../src/api';
import { ChainRulesPanel, IngressStreamRow } from '../src/panes/chains';

const chain: SnapshotChain = { id: 'chain-1', tenant: 'platform', name: '访客链路' };
const step: SnapshotStep = {
  chain: chain.id,
  node: 'entry',
  accept: null,
  hop_in: null,
  rules: [],
};
const ingress: SnapshotIngress = {
  id: 'ingress-1',
  chain: chain.id,
  node: 'entry',
  bind: '0.0.0.0',
  port: 443,
  projection: null,
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
  },
  identity: { public_key: 'public-key', short_ids: ['0123456789abcdef'] },
  wires: { vless: { kind: 'vless-reality' } },
};

async function visitorClientWithCachedForbidden(): Promise<QueryClient> {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  client.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['revisions'], { current_revision: null });
  await client
    .fetchQuery({
      queryKey: ['settings'],
      queryFn: async () => {
        throw new Error('forbidden');
      },
    })
    .catch(() => undefined);
  return client;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('访客查看链路规则', () => {
  it('不把共享设置缓存中的 403 显示成链路规则错误', async () => {
    const client = await visitorClientWithCachedForbidden();
    const fetcher = vi.fn(async () => {
      throw new Error('访客视角不应再发请求');
    });
    vi.stubGlobal('fetch', fetcher);

    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel
          appId="app-1"
          chain={chain}
          spine={['entry']}
          steps={[step]}
          nodes={[]}
          readOnly
          settingsReadable={false}
        />
      </QueryClientProvider>,
    );

    expect(view.queryByText('forbidden')).toBeNull();
    expect(view.getByText('entry')).toBeTruthy();
    await waitFor(() => expect(fetcher).not.toHaveBeenCalled());
  });

  it('接入面只读渲染不再尝试读取 settings', async () => {
    const client = await visitorClientWithCachedForbidden();
    const fetcher = vi.fn(async () => {
      throw new Error('访客视角不应再发请求');
    });
    vi.stubGlobal('fetch', fetcher);

    const view = render(
      <QueryClientProvider client={client}>
        <IngressStreamRow
          appId="app-1"
          ingress={ingress}
          editable={false}
          settingsReadable={false}
          section="vless"
        />
      </QueryClientProvider>,
    );

    expect(view.getByText('REALITY 目标站点')).toBeTruthy();
    expect(view.getByRole('option', { name: '全局站点' })).toBeTruthy();
    expect(view.queryByRole('option', { name: '全局站点（未配置）' })).toBeNull();
    await waitFor(() => expect(fetcher).not.toHaveBeenCalled());
  });
});
