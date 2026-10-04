import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ConsoleSnapshot, GrantWrite, SnapshotIngress, UserListItem } from '../src/api';
import { draft } from '../src/draft';
import { UsersPane } from '../src/panes/users';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

const user: UserListItem = {
  tenant_id: 'platform.acme',
  id: 'alice',
  status: 'active',
  created_at: '2026-09-28T00:00:00Z',
  created_revision: 1,
};
const win: Win = {
  id: 1,
  key: 'tab:users',
  title: '用户',
  x: 0,
  y: 0,
  w: 1200,
  h: 900,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'user', id: 'alice' } },
};
const snapshot = (revision: number, granted: string[]): ConsoleSnapshot => ({
  snapshot: {
    revision,
    apps: [
      {
        id: 'app-main',
        label: '测试线路',
        chains: [],
        steps: [],
        fronts: [],
        ingresses: ['in-a', 'in-b'].map(
          (id, i) => ({ id, chain: `chain-${i}`, node: 'n1', port: 443 }) as SnapshotIngress,
        ),
        grants: granted.map(ingress => ({ tenant: user.tenant_id, user: user.id, ingress })),
      },
    ],
  },
  node_egress_dns: [],
  redacted: false,
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => {
    resolve = done;
  });
  return { promise, resolve };
}
const clients: QueryClient[] = [];
beforeEach(() => {
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
});
afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  draft.clear();
  vi.unstubAllGlobals();
});

function mount(readSnapshot: () => Promise<Response>, write: (body: GrantWrite) => Promise<Response>) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  clients.push(client);
  client.setQueryData(['snapshot'], snapshot(1, ['in-a', 'in-b']));
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['users'], { users: [user] });
  client.setQueryData(['tenants'], { tenants: [{ id: user.tenant_id, name: '默认' }] });
  client.setQueryData(['quotas'], { quotas: [] });
  client.setQueryData(['usage-monthly'], { views: [] });
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path === '/grants') return write(JSON.parse(String(init?.body)) as GrantWrite);
      if (path === '/model/snapshot') return readSnapshot();
      if (path.startsWith('/users?')) return Response.json({ users: [user] });
      if (path === '/grant-probes/capability') return Response.json({ available: false, reason: '测试中不发起拨测' });
      throw new Error(`unexpected ${path}`);
    }),
  );
  const view = render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'operator',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
          initial: { node_count: 0, chain_group_count: [] },
        }}
      >
        <UsersPane win={win} />
      </SessionProvider>
    </QueryClientProvider>,
  );
  const cards = () => [...view.container.querySelectorAll<HTMLButtonElement>('.grant-card')];
  return { client, view, cards };
}

describe('grant toggles', () => {
  it('keeps a card locked after POST until the snapshot confirms the returned revision', async () => {
    const fresh = deferred<Response>();
    const read = vi.fn(() => fresh.promise);
    const write = vi.fn(async (_body: GrantWrite) => Response.json({ revision_id: 2 }));
    const { cards } = mount(read, write);
    fireEvent.click(cards()[0]);
    await waitFor(() => expect(read).toHaveBeenCalledTimes(1));
    expect(cards()[0].disabled).toBe(true);
    expect(cards()[0].getAttribute('aria-pressed')).toBe('true');
    await act(async () => fresh.resolve(Response.json(snapshot(2, ['in-b']))));
    await waitFor(() => expect(cards()[0].disabled).toBe(false));
    expect(cards()[0].getAttribute('aria-pressed')).toBe('false');
    fireEvent.click(cards()[0]);
    await waitFor(() => expect(write).toHaveBeenCalledTimes(2));
    expect(write.mock.calls.map(([body]) => body.enabled)).toEqual([false, true]);
  });

  it('keeps A locked while B is submitted and settles independently', async () => {
    const a = deferred<Response>();
    const b = deferred<Response>();
    const fresh = deferred<Response>();
    const write = vi.fn((body: GrantWrite) => (body.ingress_id === 'in-a' ? a.promise : b.promise));
    const { cards } = mount(() => fresh.promise, write);
    fireEvent.click(cards()[0]);
    fireEvent.click(cards()[1]);
    await waitFor(() => expect(cards().map(card => card.disabled)).toEqual([true, true]));
    await act(async () => a.resolve(Response.json({ revision_id: 2 })));
    expect(cards().map(card => card.disabled)).toEqual([true, true]);
    await act(async () => fresh.resolve(Response.json(snapshot(2, ['in-b']))));
    await waitFor(() => expect(cards()[0].disabled).toBe(false));
    expect(cards()[1].disabled).toBe(true);
    await act(async () => b.resolve(Response.json({ revision_id: 3 })));
  });

  it('does not unlock a saved write on a canceled refresh, stale response or refresh error', async () => {
    const stale = deferred<Response>();
    const read = vi.fn(() => stale.promise);
    const { client, view, cards } = mount(read, async () => Response.json({ revision_id: 2 }));
    fireEvent.click(cards()[0]);
    await waitFor(() => expect(read).toHaveBeenCalledTimes(1));
    await act(async () => client.cancelQueries({ queryKey: ['snapshot'] }));
    await act(async () => stale.resolve(Response.json(snapshot(1, ['in-a', 'in-b']))));
    expect(cards()[0].disabled).toBe(true);
    read.mockImplementation(async () => Response.json(snapshot(1, ['in-a', 'in-b'])));
    await act(async () => {
      await client.refetchQueries({ queryKey: ['snapshot'] });
    });
    expect(cards()[0].disabled).toBe(true);
    expect(cards()[0].getAttribute('aria-pressed')).toBe('true');
    read.mockImplementation(async () => Response.json({ error: '暂时读取失败' }, { status: 503 }));
    await act(async () => {
      await client.refetchQueries({ queryKey: ['snapshot'] });
    });
    expect(cards()).toHaveLength(2);
    expect(cards()[0].disabled).toBe(true);
    read.mockImplementation(async () => Response.json(snapshot(2, ['in-b'])));
    fireEvent.click(view.getByRole('button', { name: '重试读取授权' }));
    await waitFor(() => expect(cards()[0].disabled).toBe(false));
    expect(cards()[0].getAttribute('aria-pressed')).toBe('false');
  });
});
