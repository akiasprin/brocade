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
  projection: {},
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
        <IngressStreamRow appId="app-1" ingress={ingress} editable={false} settingsReadable={false} section="vless" />
      </QueryClientProvider>,
    );

    expect(view.getByText('REALITY 目标站点')).toBeTruthy();
    expect(view.getByRole('option', { name: '全局站点' })).toBeTruthy();
    expect(view.queryByRole('option', { name: '全局站点（未配置）' })).toBeNull();
    await waitFor(() => expect(fetcher).not.toHaveBeenCalled());
  });

  it('折叠摘要只显示普通跳实际监听方自己的端口', async () => {
    const client = await visitorClientWithCachedForbidden();
    const steps: SnapshotStep[] = [
      {
        chain: chain.id,
        node: 'entry',
        accept: null,
        // The entry does not listen for this ordinary forward hop.
        hop_in: { port: 20002, security: { t: 'none' } },
        rules: [
          {
            m: { t: 'any' },
            a: {
              t: 'forward',
              to: 'target',
              dial: { t: 'addr', v: '192.0.2.2:20000' },
              pool: { t: 'none' },
            },
          },
        ],
      },
      {
        chain: chain.id,
        node: 'target',
        accept: null,
        hop_in: { port: 20000, security: { t: 'none' } },
        rules: [],
      },
    ];

    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel
          appId="app-1"
          chain={chain}
          spine={['entry', 'target']}
          steps={steps}
          nodes={[]}
          readOnly
          settingsReadable={false}
        />
      </QueryClientProvider>,
    );

    expect(
      [...view.container.querySelectorAll('.chain-rule-node-head .m-hop > .mono')].map(node => node.textContent),
    ).toEqual(['—', '20000 / VLESS-NONE']);
  });

  it('反向跳只在上游监听方显示一次端口', async () => {
    const client = await visitorClientWithCachedForbidden();
    const steps: SnapshotStep[] = [
      {
        chain: chain.id,
        node: 'entry',
        accept: null,
        hop_in: { port: 20002, security: { t: 'encryption', v: { public_key: 'entry-public' } } },
        rules: [
          {
            m: { t: 'any' },
            a: { t: 'forward', to: 'target', dial: { t: 'reverse', v: 'v4' }, pool: { t: 'none' } },
          },
        ],
      },
      {
        chain: chain.id,
        node: 'target',
        accept: null,
        // Historical residue from when the same edge was normal.
        hop_in: { port: 20001, security: { t: 'none' } },
        rules: [],
      },
    ];

    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel
          appId="app-1"
          chain={chain}
          spine={['entry', 'target']}
          steps={steps}
          nodes={[]}
          readOnly
          settingsReadable={false}
        />
      </QueryClientProvider>,
    );

    expect(
      [...view.container.querySelectorAll('.chain-rule-node-head .m-hop > .mono')].map(node => node.textContent),
    ).toEqual(['20002 / VLESS-ENCRY', '—']);
  });
});
