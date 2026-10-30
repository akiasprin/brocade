import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { UserCycleReset } from '../src/panes/user-cycle-reset';
import type { UserCyclePeriod, UserListItem } from '../src/api';

const user: UserListItem = {
  tenant_id: 'platform',
  id: 'olivia',
  status: 'active',
  created_at: '',
  created_revision: 1,
  traffic_reset_day: 29,
};
const period: UserCyclePeriod = {
  tenant_id: 'platform',
  user_id: 'olivia',
  starts_at: '2026-10-28T16:00:00Z',
  ends_at: '2026-11-28T16:00:00Z',
  reset_id: 12,
};
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function mount(editable = true, activePeriod: UserCyclePeriod | null = period) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  const invalidated = vi.spyOn(client, 'invalidateQueries');
  const view = render(
    <QueryClientProvider client={client}>
      <UserCycleReset user={user} period={activePeriod ?? undefined} editable={editable} />
    </QueryClientProvider>,
  );
  const open = () => {
    fireEvent.click(view.getByRole('button', { name: '重置' }));
    return within(view.getByRole('dialog', { name: 'olivia 的本期用量归零' }));
  };
  return { ...view, open, invalidated };
}

describe('用户本期用量归零', () => {
  it('HTTP IP 部署缺少 randomUUID 时仍能确认归零', async () => {
    vi.stubGlobal('crypto', {
      getRandomValues: (value: Uint8Array) => value.fill(0x23),
    });
    const fetcher = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ reset_id: 13, reset_at: '2026-10-29T07:00:00Z', duplicate: false }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      );
    vi.stubGlobal('fetch', fetcher);
    const view = mount();
    view.open();
    fireEvent.click(view.getByRole('button', { name: '确认本期归零' }));
    await waitFor(() => expect(view.queryByRole('dialog')).toBeNull());
    expect(JSON.parse(String(fetcher.mock.calls[0][1].body)).request_id).toMatch(/^[0-9a-f]{32}$/);
  });

  it('只读隐藏操作，缺少账期时禁用，取消不发请求', () => {
    const fetcher = vi.fn();
    vi.stubGlobal('fetch', fetcher);
    const readonly = mount(false);
    expect(readonly.queryByRole('button')).toBeNull();
    readonly.unmount();
    const pending = mount(true, null);
    expect((pending.getByRole('button', { name: '重置' }) as HTMLButtonElement).disabled).toBe(true);
    pending.unmount();
    const view = mount();
    const dialog = view.open();
    expect(dialog.getByText(/历史用量与原始计量记录保留/).textContent).toContain('每月 29 日');
    fireEvent.click(dialog.getByRole('button', { name: '取消' }));
    expect(view.queryByRole('dialog')).toBeNull();
    expect(fetcher).not.toHaveBeenCalled();
  });

  it('确认后发送并发标识，并刷新页面、订阅和两类额度', async () => {
    let done!: (response: Response) => void;
    const fetcher = vi.fn(
      () =>
        new Promise<Response>(resolve => {
          done = resolve;
        }),
    );
    vi.stubGlobal('fetch', fetcher);
    const view = mount();
    const dialog = view.open();
    fireEvent.click(dialog.getByRole('button', { name: '确认本期归零' }));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    const [url, init] = fetcher.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('/users/platform/olivia/cycle-usage/reset');
    expect(init.method).toBe('POST');
    expect(JSON.parse(String(init.body))).toMatchObject({
      expected_reset_id: 12,
      request_id: expect.stringMatching(/^[0-9a-f]{32}$/),
    });
    expect((dialog.getByRole('button', { name: '归零中…' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(view.getByRole('dialog')).toBeTruthy();
    done(
      new Response(JSON.stringify({ reset_id: 13, reset_at: '2026-10-29T07:00:00Z', duplicate: false }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    );
    await waitFor(() => expect(view.queryByRole('dialog')).toBeNull());
    const keys = view.invalidated.mock.calls.map(([arg]) => arg?.queryKey?.[0]);
    expect(keys).toEqual(
      expect.arrayContaining(['user-cycles', 'quotas', 'forwarding', 'user-traffic-policy', 'clash-subscription']),
    );
  });

  it('响应丢失后重试沿用同一标识，后台变化不替换已确认的并发前提', async () => {
    const fetcher = vi
      .fn()
      .mockRejectedValueOnce(new Error('连接中断'))
      .mockResolvedValue(
        new Response(JSON.stringify({ reset_id: 13, reset_at: '2026-10-29T07:00:00Z', duplicate: true }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      );
    vi.stubGlobal('fetch', fetcher);
    const view = mount();
    view.open();
    fireEvent.click(view.getByRole('button', { name: '确认本期归零' }));
    await waitFor(() => expect(view.getByText(/连接中断/)).toBeTruthy());
    view.rerender(
      <QueryClientProvider client={clients.at(-1)!}>
        <UserCycleReset
          user={user}
          period={{ ...period, reset_id: 99, starts_at: '2026-11-28T16:00:00Z', ends_at: '2026-12-28T16:00:00Z' }}
          editable
        />
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('button', { name: '确认本期归零' }));
    await waitFor(() => expect(view.queryByRole('dialog')).toBeNull());
    const bodies = fetcher.mock.calls.map(([, init]) => JSON.parse(String(init.body)));
    expect(bodies[1]).toEqual(bodies[0]);
    expect(bodies[0].expected_reset_id).toBe(12);
    expect(bodies[0].expected_period_start).toBe(period.starts_at);
    expect(bodies[0].expected_period_end).toBe(period.ends_at);
  });
});
