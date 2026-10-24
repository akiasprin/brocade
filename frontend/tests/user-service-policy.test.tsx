import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { UserServiceTerms, extendServiceTime } from '../src/panes/user-service-policy';
import type { UserListItem } from '../src/api';

const user: UserListItem = {
  tenant_id: 'platform.acme',
  id: 'alice',
  status: 'active',
  created_at: '2026-01-01T00:00:00Z',
  created_revision: 1,
  traffic_reset_day: 1,
  service_starts_at: null,
  service_expires_at: null,
  service_state: 'active',
};
const preview = {
  starts_at: '2026-09-14T16:00:00Z',
  ends_at: '2026-10-14T16:00:00Z',
  access_used_bytes: 1024 ** 3,
  forwarding_used_bytes: 2 * 1024 ** 3,
  service_state: 'active',
};
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.forEach(c => c.clear());
  clients.length = 0;
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function mount(value = user, editable = true) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  const saved = vi.fn();
  const view = render(
    <QueryClientProvider client={client}>
      <UserServiceTerms user={value} editable={editable} onSaved={saved} />
    </QueryClientProvider>,
  );
  const terms = () => view.getByRole('button', { name: /^重置日/ });
  const open = () => {
    fireEvent.click(terms());
    return within(view.getByRole('dialog', { name: 'alice 的账期与有效期' }));
  };
  return { ...view, saved, terms, open };
}

const requests = (fetcher: ReturnType<typeof vi.fn>, method: 'POST' | 'PUT') =>
  (fetcher.mock.calls as [RequestInfo | URL, RequestInit | undefined][])
    .filter(([, init]) => init?.method === method)
    .map(([url, init]) => ({ url: String(url), body: JSON.parse(String(init?.body)) }));

describe('用户账期与有效期', () => {
  it('按目标月末续期，并保持 UTC+8 时刻', () => {
    expect(new Date(extendServiceTime(Date.parse('2026-01-31T09:30:00+08:00'), 'months')).toISOString()).toBe(
      '2026-02-28T01:30:00.000Z',
    );
    expect(new Date(extendServiceTime(Date.parse('2028-02-29T09:30:00+08:00'), 'years')).toISOString()).toBe(
      '2029-02-28T01:30:00.000Z',
    );
    expect(new Date(extendServiceTime(Date.parse('2026-01-31T09:30:00+08:00'), 'days')).toISOString()).toBe(
      '2026-03-02T01:30:00.000Z',
    );
  });

  it('只读身份在读数栏看到重置日和有效期，不开放修改', () => {
    const view = mount(
      { ...user, traffic_reset_day: 31, service_state: 'expired', service_expires_at: '2026-09-30T16:00:00Z' },
      false,
    );
    const terms = view.container.querySelector('dl.user-service-terms')!;
    expect([...terms.querySelectorAll('dt')].map(term => term.textContent)).toEqual(['重置日', '有效期']);
    expect(within(terms as HTMLElement).getByText('每月 31 日')).toBeTruthy();
    expect(
      within(terms as HTMLElement)
        .getByText('2026-10-01 已到期')
        .classList.contains('err'),
    ).toBe(true);
    expect(terms.getAttribute('title')).toBe('每月 31 日重置 · 立即生效 · 2026-10-01 00:00 到期（UTC+8）');
    expect(view.queryByRole('button')).toBeNull();
  });

  it('有效期常态写到期日，七天内到期和未生效标为注意', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(new Date('2026-10-22T12:00:00+08:00'));
    const view = mount(user, false);
    const value = () => view.container.querySelectorAll('dd')[1];
    expect(value().textContent).toBe('长期有效');
    const rerender = (next: Partial<UserListItem>) =>
      view.rerender(
        <QueryClientProvider client={clients[0]}>
          <UserServiceTerms user={{ ...user, ...next }} editable={false} onSaved={() => undefined} />
        </QueryClientProvider>,
      );
    rerender({ service_expires_at: '2027-01-14T16:00:00Z' });
    await waitFor(() => expect(value().textContent).toBe('2027-01-15 到期'));
    expect(value().className).toBe('user-service-term-value');
    rerender({ service_expires_at: '2026-10-24T16:00:00Z' });
    expect(value().textContent).toBe('3 天后到期');
    expect(value().classList.contains('gold')).toBe(true);
    rerender({ service_state: 'pending', service_starts_at: '2026-10-31T16:00:00Z', service_expires_at: null });
    expect(value().textContent).toBe('2026-11-01 起生效');
    expect(value().classList.contains('gold')).toBe(true);
  });

  it('没有改动也能直接保存，不请求预览', async () => {
    const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) =>
      Response.json(init?.method === 'PUT' ? user : preview),
    );
    vi.stubGlobal('fetch', fetcher);
    const view = mount();
    const dialog = view.open();
    expect(view.terms().getAttribute('aria-expanded')).toBe('true');
    expect(document.activeElement).toBe(dialog.getByLabelText('每月流量重置日'));
    expect(dialog.queryByText(/^本期/)).toBeNull();
    const save = dialog.getByRole('button', { name: '保存' }) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() => expect(view.saved).toHaveBeenCalledOnce());
    await waitFor(() => expect(view.queryByRole('dialog')).toBeNull());
    expect(requests(fetcher, 'POST')).toEqual([]);
    expect(requests(fetcher, 'PUT')).toEqual([
      {
        url: '/users/platform.acme/alice/service-policy',
        body: { traffic_reset_day: 1, service_starts_at: null, service_expires_at: null },
      },
    ]);
    expect(view.terms().getAttribute('aria-expanded')).toBe('false');
  });

  it('修改后停顿片刻自动预览新账期和用量，保存不等预览', async () => {
    const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) =>
      Response.json(init?.method === 'PUT' ? { ...user, traffic_reset_day: 15 } : preview),
    );
    vi.stubGlobal('fetch', fetcher);
    const view = mount();
    const dialog = view.open();
    const day = dialog.getByLabelText('每月流量重置日');
    fireEvent.change(day, { target: { value: '3' } });
    fireEvent.change(day, { target: { value: '15' } });
    expect((dialog.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(false);
    await waitFor(() => expect(dialog.getByText('本期 09-15 → 10-15')).toBeTruthy());
    expect(dialog.getByText('接入 1.00 GiB')).toBeTruthy();
    expect(dialog.getByText('转发 2.00 GiB')).toBeTruthy();
    expect(requests(fetcher, 'POST')).toEqual([
      {
        url: '/users/platform.acme/alice/service-policy/preview',
        body: { traffic_reset_day: 15, service_starts_at: null, service_expires_at: null },
      },
    ]);
    fireEvent.change(day, { target: { value: '1' } });
    expect(dialog.queryByText(/^本期/)).toBeNull();
    fireEvent.change(day, { target: { value: '31' } });
    fireEvent.click(dialog.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(view.saved).toHaveBeenCalledOnce());
    expect(requests(fetcher, 'PUT').map(write => write.body)).toEqual([
      { traffic_reset_day: 31, service_starts_at: null, service_expires_at: null },
    ]);
  });

  it('输入无效时说明原因并禁用保存，预览失败时保留输入', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ error: '预览暂不可用' }, { status: 503 })),
    );
    const view = mount();
    const dialog = view.open();
    const save = dialog.getByRole('button', { name: '保存' }) as HTMLButtonElement;
    const day = dialog.getByLabelText('每月流量重置日') as HTMLInputElement;
    fireEvent.change(day, { target: { value: '32' } });
    expect(dialog.getByRole('alert').textContent).toBe('重置日为 1–31 日');
    expect(day.getAttribute('aria-invalid')).toBe('true');
    expect(save.disabled).toBe(true);
    fireEvent.change(day, { target: { value: '15' } });
    fireEvent.click(within(dialog.getByRole('group', { name: '生效方式' })).getByRole('button', { name: '指定时间' }));
    fireEvent.change(dialog.getByLabelText('生效时间（UTC+8）'), { target: { value: '2026-10-01T00:00' } });
    fireEvent.click(within(dialog.getByRole('group', { name: '到期方式' })).getByRole('button', { name: '指定时间' }));
    fireEvent.change(dialog.getByLabelText('到期时间（UTC+8）'), { target: { value: '2026-10-01T00:00' } });
    expect(dialog.getByRole('alert').textContent).toBe('到期须晚于生效');
    expect(save.disabled).toBe(true);
    fireEvent.click(within(dialog.getByRole('group', { name: '到期方式' })).getByRole('button', { name: '长期' }));
    expect(dialog.queryByRole('alert')).toBeNull();
    expect(save.disabled).toBe(false);
    await waitFor(() => expect(dialog.getByText('预览暂不可用')).toBeTruthy());
    expect(day.value).toBe('15');
  });

  it('只修改重置日时，保持已保存时间的秒数和时区', async () => {
    const fetcher = vi.fn(async () => Response.json(preview));
    vi.stubGlobal('fetch', fetcher);
    const view = mount({ ...user, service_expires_at: '2099-01-01T00:00:45Z' });
    const dialog = view.open();
    expect((dialog.getByLabelText('到期时间（UTC+8）') as HTMLInputElement).value).toBe('2099-01-01T08:00');
    fireEvent.change(dialog.getByLabelText('每月流量重置日'), { target: { value: '15' } });
    await waitFor(() => expect(fetcher).toHaveBeenCalledOnce());
    expect(requests(fetcher, 'POST')[0].body.service_expires_at).toBe('2099-01-01T00:00:45Z');
  });

  it('续期从当前填写的到期时间往后算，按月取目标月末', () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json(preview)),
    );
    const view = mount({ ...user, service_expires_at: '2099-01-31T01:30:00Z' });
    const dialog = view.open();
    const end = dialog.getByLabelText('到期时间（UTC+8）') as HTMLInputElement;
    const renew = within(dialog.getByRole('group', { name: '续期' }));
    fireEvent.click(renew.getByRole('button', { name: '+1 个月' }));
    expect(end.value).toBe('2099-02-28T09:30');
    fireEvent.click(renew.getByRole('button', { name: '+1 个月' }));
    expect(end.value).toBe('2099-03-28T09:30');
    fireEvent.click(renew.getByRole('button', { name: '+30 天' }));
    expect(end.value).toBe('2099-04-27T09:30');
    fireEvent.click(renew.getByRole('button', { name: '+1 年' }));
    expect(end.value).toBe('2100-04-27T09:30');
  });

  it('给已到期用户续期时，预览标出恢复服务', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json(preview)),
    );
    const view = mount({ ...user, service_state: 'expired', service_expires_at: '2026-09-30T16:00:00Z' });
    expect(view.terms().textContent).toContain('2026-10-01 已到期');
    const dialog = view.open();
    fireEvent.click(within(dialog.getByRole('group', { name: '续期' })).getByRole('button', { name: '+1 年' }));
    expect(
      within(dialog.getByRole('group', { name: '到期方式' }))
        .getByRole('button', { name: '指定时间' })
        .getAttribute('aria-pressed'),
    ).toBe('true');
    await waitFor(() => expect(dialog.getByText('恢复服务').classList.contains('ok')).toBe(true));
  });
});
