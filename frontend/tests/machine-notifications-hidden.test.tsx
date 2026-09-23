import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { NodesPane } from '../src/panes/nodes';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it.each([false, true])('机器列表不显示通知，也不请求通知接口（纸面布局：%s）', async bare => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['machine-notifications'], {
    retention_days: 90,
    events: [
      {
        id: 1,
        node_id: 'test-node',
        node_name: '测试机器',
        event_kind: 'node_offline',
        family: null,
        previous_value: 'online',
        current_value: 'offline',
        occurred_at: '2026-09-21T00:00:00Z',
      },
    ],
  });
  const fetcher = vi.fn(
    async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(JSON.stringify({ nodes: [] }), { status: 200, headers: { 'content-type': 'application/json' } }),
  );
  vi.stubGlobal('fetch', fetcher);
  const win: Win = {
    id: 1,
    key: 'tab:nodes',
    title: '机器',
    x: 0,
    y: 0,
    w: 800,
    h: 600,
    z: 1,
    min: false,
    home: 'desk',
    data: { crumb: [] },
  };

  render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'admin',
            role: 'system-admin',
            tenant_scope: 'platform',
            token_prefix: null,
            masked_assets: false,
          },
          initial: { node_count: 0, chain_group_count: [] },
        }}
      >
        <NodesPane win={win} bare={bare} />
      </SessionProvider>
    </QueryClientProvider>,
  );

  expect(screen.getByText('还没有机器')).toBeTruthy();
  expect(screen.queryByText('机器通知')).toBeNull();
  expect(screen.queryByText('测试机器')).toBeNull();
  await waitFor(() => expect(fetcher).toHaveBeenCalledWith('/nodes/agent-state', expect.anything()));
  expect(fetcher.mock.calls.some(([url]) => String(url).startsWith('/notifications'))).toBe(false);
});
