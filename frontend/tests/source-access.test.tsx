import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { SourceAccessPanel } from '../src/panes/source-access';

const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function mount(editable = true) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  clients.push(client);
  return render(
    <QueryClientProvider client={client}>
      <SourceAccessPanel nodeId="n1" editable={editable} />
    </QueryClientProvider>,
  );
}
async function toggle(view: ReturnType<typeof mount>) {
  const summary = view.getByText('来源 IP 封禁').closest('summary')!;
  const expanding = !(summary.parentElement as HTMLDetailsElement).open;
  fireEvent.click(summary);
  await waitFor(() => expect(within(summary).getByText(expanding ? '收起' : '展开')).toBeTruthy());
}
const path = '/nodes/n1/source-access';
const initial = { revision: 7, ips: ['2001:db8::1'], nodes: [{ node_id: 'n1', phase: 'applied' }] };
const reads = (fetcher: ReturnType<typeof vi.fn>) =>
  fetcher.mock.calls.filter(([url, init]) => String(url) === path && !(init as RequestInit | undefined)?.method).length;

it('confirms the machine scope, saves with the current revision and removes a ban', async () => {
  let current = initial;
  const fetcher = vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'PUT')
      current = { ...current, ...JSON.parse(String(init.body)), revision: current.revision + 1 };
    return new Response(JSON.stringify(current), { status: 200 });
  });
  vi.stubGlobal('fetch', fetcher);
  const view = mount();
  expect(view.queryByRole('textbox', { name: '来源 IP' })).toBeNull();
  await toggle(view);
  await view.findByText('2001:db8::1');
  expect(view.getByText(/已生效 1\/1 台/)).toBeTruthy();
  fireEvent.change(view.getByRole('textbox', { name: '来源 IP' }), { target: { value: '192.0.2.1' } });
  await toggle(view);
  expect(view.getByRole('group', { name: '来源 IP 封禁' }).hasAttribute('open')).toBe(false);
  await toggle(view);
  expect((view.getByRole('textbox', { name: '来源 IP' }) as HTMLInputElement).value).toBe('192.0.2.1');
  fireEvent.click(view.getByRole('button', { name: '封禁 IP' }));
  const dialog = within(view.getByRole('dialog', { name: '封禁来源 IP' }));
  expect(dialog.getByText(/此机器上该来源的代理会话/)).toBeTruthy();
  expect(fetcher.mock.calls.filter(([, init]) => init?.method === 'PUT')).toHaveLength(0);
  fireEvent.click(dialog.getByRole('button', { name: '确认封禁' }));
  await view.findByText('192.0.2.1');
  expect(String(fetcher.mock.calls.find(([, init]) => init?.method === 'PUT')![0])).toBe(path);
  expect(JSON.parse(String(fetcher.mock.calls.find(([, init]) => init?.method === 'PUT')![1]!.body))).toEqual({
    revision: 7,
    ips: ['2001:db8::1', '192.0.2.1'],
  });
  expect((view.getByRole('textbox', { name: '来源 IP' }) as HTMLInputElement).value).toBe('');
  fireEvent.click(view.getByRole('button', { name: '解除封禁 192.0.2.1' }));
  await waitFor(() => expect(view.queryByText('192.0.2.1')).toBeNull());
  expect(current.ips).toEqual(['2001:db8::1']);
});

it('shows unsupported nodes and offers no write controls to a reader', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify({ ...initial, nodes: [{ node_id: 'n1', phase: 'unsupported' }] }))),
  );
  const view = mount(false);
  await toggle(view);
  await view.findByText(/部分节点需要升级/);
  expect(view.queryByRole('textbox')).toBeNull();
  expect(view.queryByRole('button', { name: /解除/ })).toBeNull();
});

it('retains the submitted address after a conflict and refreshes the authoritative list', async () => {
  const fetcher = vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) =>
    init?.method === 'PUT'
      ? Response.json({ error: '封禁名单已被其他操作修改，请刷新后重试' }, { status: 409 })
      : new Response(JSON.stringify(initial)),
  );
  vi.stubGlobal('fetch', fetcher);
  const view = mount();
  await toggle(view);
  await view.findByText('2001:db8::1');
  fireEvent.change(view.getByRole('textbox', { name: '来源 IP' }), { target: { value: '192.0.2.1' } });
  fireEvent.click(view.getByRole('button', { name: '封禁 IP' }));
  fireEvent.click(within(view.getByRole('dialog')).getByRole('button', { name: '确认封禁' }));
  await waitFor(() => expect(reads(fetcher)).toBeGreaterThan(1));
  expect(view.getAllByText('封禁名单已被其他操作修改，请刷新后重试').length).toBeGreaterThan(0);
  expect((view.getByRole('textbox', { name: '来源 IP' }) as HTMLInputElement).value).toBe('192.0.2.1');
});

it('does not turn a missing or malformed API into an empty successful list', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response('{}')),
  );
  const view = mount();
  await toggle(view);
  await view.findByText(/来源封禁响应无效/);
  expect(view.queryByText('暂无封禁地址')).toBeNull();
  expect(view.queryByRole('button', { name: '封禁 IP' })).toBeNull();
});

it('reads the list every 3 s while a machine has yet to confirm it and every 30 s otherwise', async () => {
  vi.useFakeTimers();
  let current = { ...initial, nodes: [{ node_id: 'n1', phase: 'pending' }] };
  const fetcher = vi.fn(async () => Response.json(current));
  vi.stubGlobal('fetch', fetcher);
  const view = mount();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(view.getByText('1 个地址')).toBeTruthy();
  const first = reads(fetcher);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(3_000);
  });
  expect(reads(fetcher)).toBe(first + 1);

  current = initial;
  await act(async () => {
    await vi.advanceTimersByTimeAsync(3_000);
  });
  const settled = reads(fetcher);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(27_000);
  });
  expect(reads(fetcher)).toBe(settled);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(3_000);
  });
  expect(reads(fetcher)).toBe(settled + 1);
});
