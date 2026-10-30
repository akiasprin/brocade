import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { SourceAccessPanel } from '../src/panes/source-access';
import type { SourceAccessView, UserPresenceList } from '../src/api';

const navigation = vi.hoisted(() => vi.fn());
vi.mock('../src/forge/route', () => ({ navigate: navigation }));
const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  navigation.mockClear();
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
const path = '/nodes/n1/source-access';
const initial: SourceAccessView = {
  revision: 7,
  ips: ['2001:db8::1'],
  nodes: [{ node_id: 'n1', phase: 'applied' }],
  user_bans: [],
};
const emptyPresence: UserPresenceList = { freshness_secs: 120, users: [] };
const reads = (fetcher: ReturnType<typeof vi.fn>, url = path) =>
  fetcher.mock.calls.filter(([input, init]) => String(input) === url && !(init as RequestInit | undefined)?.method)
    .length;
function backend(value = initial, presence = emptyPresence) {
  let current = structuredClone(value);
  const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    if (String(input) === '/users/presence') return Response.json(presence);
    if (init?.method === 'PUT')
      current = {
        ...current,
        ...JSON.parse(String(init.body)),
        revision: current.revision + 1,
        nodes: [{ node_id: 'n1', phase: 'pending' }],
      };
    return Response.json(current);
  });
  vi.stubGlobal('fetch', fetcher);
  return { fetcher, current: () => current };
}
async function openAdd(view: ReturnType<typeof mount>) {
  fireEvent.click(await view.findByRole('button', { name: '封禁 IP' }));
  return view.getByRole('textbox', { name: '来源 IP' });
}

it('always shows this machine, confirms inline with its revision and lifts in one click', async () => {
  const { fetcher, current } = backend();
  const view = mount();
  await view.findByText('2001:db8::1');
  expect(view.getByRole('region', { name: '来源 IP 封禁' }).tagName).toBe('SECTION');
  expect(view.getByRole('status').textContent).toBe('已生效');
  expect(view.queryByRole('textbox')).toBeNull();
  expect(reads(fetcher, '/users/presence')).toBe(0);
  const input = await openAdd(view);
  fireEvent.change(input, { target: { value: ' 192.0.2.1 ' } });
  expect(view.getByText('对本机所有用户与端口转发生效')).toBeTruthy();
  fireEvent.click(view.getByRole('button', { name: /^封禁$/ }));
  expect(view.queryByRole('dialog')).toBeNull();
  await view.findByText('192.0.2.1');
  expect(view.getByRole('status').textContent).toBe('同步中');
  const put = fetcher.mock.calls.find(([, init]) => init?.method === 'PUT')!;
  expect(put[0]).toBe(path);
  expect(JSON.parse(String(put[1]!.body))).toEqual({ revision: 7, ips: ['2001:db8::1', '192.0.2.1'] });
  expect(view.queryByRole('textbox')).toBeNull();
  fireEvent.click(view.getByRole('button', { name: '解除本机封禁 192.0.2.1' }));
  await waitFor(() => expect(view.queryByText('192.0.2.1')).toBeNull());
  expect(current().ips).toEqual(['2001:db8::1']);
});

it('summarizes users, expands read-only regions and navigates; lifting here preserves user bans', async () => {
  const { current } = backend({
    ...initial,
    user_bans: [
      { tenant_id: 'platform.acme', user_id: 'alice', ips: ['2001:db8::1'] },
      { tenant_id: 'platform.other', user_id: 'alice', ips: ['192.0.2.1'] },
    ],
    source_countries: [{ ip: '192.0.2.1', country: 'CN' }],
    source_operators: [{ ip: '192.0.2.1', operator: 'chinanet' }],
  });
  const view = mount();
  const toggle = await view.findByRole('button', { name: /2 位用户另有 2 个封禁/ });
  expect(toggle.getAttribute('aria-expanded')).toBe('false');
  expect(view.queryByRole('list', { name: '用户封禁' })).toBeNull();
  fireEvent.click(toggle);
  const users = within(view.getByRole('list', { name: '用户封禁' }));
  expect(users.getAllByText('alice')).toHaveLength(2);
  expect(users.getByText('platform.other')).toBeTruthy();
  expect(users.getByText('中国电信')).toBeTruthy();
  expect(users.queryByRole('button', { name: /解除/ })).toBeNull();
  fireEvent.click(users.getAllByRole('button', { name: 'alice' })[0]);
  expect(navigation).toHaveBeenCalledWith('users', { p: 'user', id: 'alice' });
  fireEvent.click(view.getByRole('button', { name: '解除本机封禁 2001:db8::1' }));
  await view.findByText('本机未封禁任何来源');
  expect(users.getByText('2001:db8::1')).toBeTruthy();
  expect(current().user_bans).toHaveLength(2);
});

it('normalizes duplicates and only warns about users online on this machine', async () => {
  const source = (ip: string, node_ids: string[]) => ({
    ip,
    node_ids,
    ingress_ids: [],
    first_observed_at: '',
    last_observed_at: '',
    xray_last_seen_at: '',
  });
  const { fetcher } = backend(
    { ...initial, user_bans: [{ tenant_id: 'platform', user_id: 'bob', ips: ['192.0.2.2', '192.0.2.3'] }] },
    {
      freshness_secs: 120,
      users: [
        {
          tenant_id: 'platform',
          user_id: 'alice',
          state: 'complete',
          reporting_nodes: 1,
          expected_nodes: 1,
          sources: [source('192.0.2.2', ['n1']), source('192.0.2.3', ['n2'])],
        },
      ],
    },
  );
  const view = mount();
  const input = await openAdd(view);
  await waitFor(() => expect(reads(fetcher, '/users/presence')).toBe(1));
  fireEvent.change(input, { target: { value: '2001:0DB8:0:0:0:0:0:1' } });
  expect(view.getByText('已在本机名单中')).toBeTruthy();
  expect((view.getByRole('button', { name: /^封禁$/ }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(input, { target: { value: '::ffff:192.0.2.2' } });
  expect(await view.findByText(/alice 正在使用，封禁后断开/)).toBeTruthy();
  fireEvent.change(input, { target: { value: '192.0.2.3' } });
  expect(view.getByText('已在 bob 的名单中；加入本机后对所有用户生效')).toBeTruthy();
  expect(view.queryByText(/正在使用，封禁后断开/)).toBeNull();
  fireEvent.keyDown(input, { key: 'Escape' });
  expect(view.queryByRole('textbox')).toBeNull();
});

it('fetches presence only while adding, including when reopened', async () => {
  vi.useFakeTimers();
  const { fetcher } = backend();
  const view = mount();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(reads(fetcher, '/users/presence')).toBe(0);
  fireEvent.click(view.getByRole('button', { name: '封禁 IP' }));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(reads(fetcher, '/users/presence')).toBe(1);
  fireEvent.click(view.getByRole('button', { name: '取消' }));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000);
  });
  expect(reads(fetcher, '/users/presence')).toBe(1);
  fireEvent.click(view.getByRole('button', { name: '封禁 IP' }));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(reads(fetcher, '/users/presence')).toBe(2);
});

it.each(['请输入有效的 IPv4 或 IPv6 地址', '封禁名单已被其他操作修改，请刷新后重试'])(
  'preserves input and refreshes after rejection: %s',
  async error => {
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
      init?.method === 'PUT'
        ? Response.json({ error }, { status: error.startsWith('请输入') ? 400 : 409 })
        : Response.json(String(input) === '/users/presence' ? emptyPresence : initial),
    );
    vi.stubGlobal('fetch', fetcher);
    const view = mount();
    const input = await openAdd(view);
    fireEvent.change(input, { target: { value: 'invalid' } });
    fireEvent.click(view.getByRole('button', { name: /^封禁$/ }));
    await view.findByText(error);
    await waitFor(() => expect(reads(fetcher)).toBeGreaterThan(1));
    expect((input as HTMLInputElement).value).toBe('invalid');
    await waitFor(() => expect(document.activeElement).toBe(input));
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(view.queryByRole('textbox')).toBeNull();
  },
);

it('reports failed online lookup without hiding the form', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) =>
      String(input) === '/users/presence'
        ? Response.json({ error: '不可用' }, { status: 503 })
        : Response.json(initial),
    ),
  );
  const view = mount();
  await openAdd(view);
  await view.findByText(/在线来源读取失败/);
  expect(view.getByRole('textbox')).toBeTruthy();
});

it.each(['unsupported', 'error'] as const)('shows %s on this machine and no write controls to readers', async phase => {
  backend({
    ...initial,
    nodes: [
      { node_id: 'other', phase: 'applied' },
      { node_id: 'n1', phase, error: '本机执行原因' },
    ],
  });
  const view = mount(false);
  await view.findByText(phase === 'unsupported' ? '这台机器需升级 Agent 与 Xray 才会执行封禁' : '本机执行原因');
  expect(view.getByRole('status').textContent).toBe(phase === 'unsupported' ? '待升级' : '执行异常');
  expect(view.queryByRole('button', { name: /封禁 IP|解除/ })).toBeNull();
});

it('distinguishes missing user-list capability from no bans and no receipt', async () => {
  backend({ ...initial, user_bans: undefined, nodes: [] });
  const view = mount();
  await view.findByText('用户名单需升级 Console 后显示');
  expect(view.queryByRole('status')).toBeNull();
  expect(view.queryByRole('button', { name: /位用户另有/ })).toBeNull();
});

it('expands long lists and disables adding at 256 addresses', async () => {
  backend({ ...initial, ips: Array.from({ length: 256 }, (_, i) => `192.0.2.${i}`) });
  const view = mount();
  await view.findByText('192.0.2.0');
  expect(view.queryByText('192.0.2.255')).toBeNull();
  fireEvent.click(view.getByRole('button', { name: '显示其余 248 个地址' }));
  expect(view.getByText('192.0.2.255')).toBeTruthy();
  expect((view.getByRole('button', { name: '本机名单已满 256 个' }) as HTMLButtonElement).disabled).toBe(true);
});

it.each([{}, { ...initial, user_bans: null }, { ...initial, user_bans: [{ ips: ['192.0.2.1'] }] }])(
  'rejects malformed responses: %j',
  async response => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json(response)),
    );
    const view = mount();
    await view.findByText(/来源封禁响应无效/);
    expect(view.queryByText('本机未封禁任何来源')).toBeNull();
    expect(view.queryByRole('button', { name: '封禁 IP' })).toBeNull();
  },
);

it('polls pending receipts every 3 s and settled lists every 30 s', async () => {
  vi.useFakeTimers();
  let current = { ...initial, nodes: [{ node_id: 'n1', phase: 'pending' }] };
  const fetcher = vi.fn(async () => Response.json(current));
  vi.stubGlobal('fetch', fetcher);
  const view = mount();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(view.getByRole('status').textContent).toBe('同步中');
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

it('keeps a ban and exposes a rejected lift for retry', async () => {
  const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) =>
    init?.method === 'PUT'
      ? Response.json({ error: '封禁名单已被其他操作修改，请刷新后重试' }, { status: 409 })
      : Response.json(initial),
  );
  vi.stubGlobal('fetch', fetcher);
  const view = mount();
  fireEvent.click(await view.findByRole('button', { name: '解除本机封禁 2001:db8::1' }));
  await view.findByText('封禁名单已被其他操作修改，请刷新后重试');
  expect(view.getByText('2001:db8::1')).toBeTruthy();
  await waitFor(() => expect(reads(fetcher)).toBeGreaterThan(1));
  expect((view.getByRole('button', { name: '解除本机封禁 2001:db8::1' }) as HTMLButtonElement).disabled).toBe(false);
});
