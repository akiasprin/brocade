import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, Whoami } from '../src/api';
import { draft } from '../src/draft';
import { NodesPane } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

const who: Whoami = {
  operator_id: 'operator',
  role: 'system-admin',
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};

const win: Win = {
  id: 1,
  key: 'tab:nodes',
  title: '机器',
  x: 0,
  y: 0,
  w: 1000,
  h: 800,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'node', id: 'n1', tab: 'chains' } },
};

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

describe('machine rule tab loading', () => {
  it('shows both rule cards before the draft-aware snapshot arrives, without a false empty count', async () => {
    let resolveSnapshot!: (response: Response) => void;
    const pendingSnapshot = new Promise<Response>(resolve => {
      resolveSnapshot = resolve;
    });
    const fetcher = vi.fn((path: string) =>
      path === '/model/snapshot' ? pendingSnapshot : Promise.resolve(Response.json({})),
    );
    vi.stubGlobal('fetch', fetcher);

    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData(['nodes'], { nodes: [{ node_id: 'n1', name: '东京' } as NodeAgentStateItem] });
    render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who, initial: { node_count: 1, chain_group_count: [] } }}>
          <NodesPane win={win} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    const panel = screen.getByRole('tabpanel', { name: '规则' });
    expect(screen.getByRole('heading', { name: '东京' })).toBeTruthy();
    expect(within(panel).getByText('DNS 解析策略')).toBeTruthy();
    expect(within(panel).getByText('链路规则')).toBeTruthy();
    expect(within(panel).getAllByRole('status')).toHaveLength(2);
    // Loading belongs inside the two existing cards, never inside another styled panel.
    expect(panel.querySelectorAll('.panel')).toHaveLength(2);
    expect(panel.querySelector('.panel .panel')).toBeNull();
    expect(within(panel).queryByText('0 条')).toBeNull();

    await act(async () => {
      resolveSnapshot(
        Response.json({
          snapshot: { revision: 1, nodes: [], apps: [], external_outbounds: [] },
          node_egress_dns: [],
          redacted: false,
        }),
      );
    });
    await waitFor(() =>
      expect(panel.querySelector('.node-chain-sheet > header .rule-sheet-meta')?.textContent).toBe('0 条'),
    );
    // The DNS card loads its editor module only after the snapshot is available.
    await act(async () => {
      await vi.dynamicImportSettled();
    });
    await waitFor(() => expect(within(panel).queryAllByRole('status')).toHaveLength(0));
    client.clear();
  });

  it('普通用户不会把无权读取的设置错误带进只读规则树', async () => {
    const fetcher = vi.fn((_path: string) => Promise.resolve(Response.json({})));
    vi.stubGlobal('fetch', fetcher);

    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData(['nodes'], { nodes: [{ node_id: 'n1', name: '东京' } as NodeAgentStateItem] });
    client.setQueryData(['snapshot'], {
      snapshot: {
        revision: 1,
        nodes: [],
        external_outbounds: [],
        apps: [
          {
            id: 'app-1',
            label: '测试项目',
            chains: [{ id: 'chain-1', tenant: 'platform', name: '测试线路' }],
            ingresses: [],
            steps: [{ chain: 'chain-1', node: 'n1', accept: null, hop_in: null, rules: [] }],
            fronts: [],
            grants: [],
          },
        ],
      },
      node_egress_dns: [],
      redacted: true,
    });
    await client
      .fetchQuery({
        queryKey: ['settings'],
        queryFn: async () => {
          throw new Error('forbidden');
        },
      })
      .catch(() => undefined);
    const user: Whoami = {
      operator_id: 'user-1',
      role: 'user',
      tenant_scope: 'platform',
      token_prefix: null,
      masked_assets: true,
    };

    render(
      <QueryClientProvider client={client}>
        <SessionProvider value={{ who: user, initial: { node_count: 1, chain_group_count: [] } }}>
          <NodesPane win={win} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    const panel = screen.getByRole('tabpanel', { name: /规则/ });
    await waitFor(() => expect(within(panel).getByText('测试线路')).toBeTruthy());
    expect(within(panel).queryByText('forbidden')).toBeNull();
    expect(fetcher.mock.calls.some(([path]) => path === '/settings')).toBe(false);
    client.clear();
  });
});
