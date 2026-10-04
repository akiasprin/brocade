import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AdminRole, ConsoleSnapshot, SnapshotIngress, UserListItem, UserPresenceList } from '../src/api';
import { draft } from '../src/draft';
import * as route from '../src/forge/route';
import { quotaStage, UsersPane } from '../src/panes/users';
import { SessionProvider } from '../src/session';
import type { Win } from '../src/wm/store';

const GiB = 1024 ** 3;
const tenant = 'platform.acme';
const person = (id: string, extra: Partial<UserListItem> = {}): UserListItem => ({
  tenant_id: tenant,
  id,
  status: 'active',
  created_at: '2026-09-28T00:00:00Z',
  created_revision: 1,
  ...extra,
});
const users = [
  person('alice'),
  person('dave'),
  person('carol', { account_type: 'test' }),
  person('erin', { status: 'disabled' }),
];
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
  data: { drill: { p: 'list' } },
};
const snapshot = {
  snapshot: {
    revision: 1,
    apps: [
      {
        id: 'tokyo',
        label: '东京',
        chains: [{ id: 'jp-iij', name: '东京 IIJ', subscription_country: 'JP' }],
        steps: [],
        fronts: [],
        ingresses: [{ id: 'tyo-iij', chain: 'jp-iij', node: 'n1', port: 443 } as SnapshotIngress],
        grants: ['alice', 'erin'].map(user => ({ tenant, user, ingress: 'tyo-iij' })),
      },
    ],
  },
  node_egress_dns: [],
  redacted: false,
} as unknown as ConsoleSnapshot;
const usage = (user: string, gib: number) => ({
  tenant_id: tenant,
  user_id: user,
  app_id: 'tokyo',
  uplink_bytes: 0,
  downlink_bytes: gib * GiB,
  has_gap: false,
});

const clients: QueryClient[] = [];
beforeEach(() => {
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === '/grant-probes/capability') {
        return Response.json({ available: false, reason: '测试中不发起拨测' });
      }
      throw new Error(`unexpected ${String(input)}`);
    }),
  );
});
afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  draft.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function mount(role: AdminRole = 'system-admin') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  clients.push(client);
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['nodes'], {
    nodes: [
      { node_id: 'n1', name: '东京测试节点' },
      { node_id: 'n2', name: '澳门测试节点' },
    ],
  });
  client.setQueryData(['users'], { users });
  client.setQueryData(['user-presence', 'system-admin'], {
    freshness_secs: 120,
    source_countries: [{ ip: '1.1.1.1', country: 'TW' }],
    users: [
      {
        tenant_id: tenant,
        user_id: 'alice',
        state: 'complete',
        expected_nodes: 1,
        reporting_nodes: 1,
        sources: [
          {
            ip: '1.1.1.1',
            first_observed_at: '2026-09-29T00:00:00Z',
            last_observed_at: '2026-09-29T00:01:00Z',
            xray_last_seen_at: '2026-09-29T00:00:58Z',
            node_ids: ['n1'],
            ingress_ids: ['tyo-iij'],
          },
        ],
      },
      {
        tenant_id: tenant,
        user_id: 'erin',
        state: 'complete',
        expected_nodes: 1,
        reporting_nodes: 1,
        sources: [],
      },
    ],
  });
  // 模拟旧版本留下的自助在线来源缓存；普通用户也不能因缓存命中而看到来源 IP。
  client.setQueryData(['user-presence', 'user'], client.getQueryData(['user-presence', 'system-admin']));
  if (role === 'user') client.setQueryData(['me-user'], person('alice'));
  client.setQueryData(['tenants'], { tenants: [{ id: tenant, name: '默认' }] });
  client.setQueryData(['quotas'], {
    quotas: [
      {
        tenant_id: tenant,
        user_id: 'alice',
        app_id: 'tokyo',
        limit_bytes: 100 * GiB,
        updated_at: '',
        suspended_ingresses: [],
      },
      {
        tenant_id: tenant,
        user_id: 'dave',
        app_id: 'tokyo',
        limit_bytes: 50 * GiB,
        updated_at: '',
        suspended_ingresses: ['tyo-iij'],
      },
    ],
  });
  client.setQueryData(['usage-monthly'], { views: [usage('alice', 54.1), usage('dave', 50.41), usage('erin', 12.3)] });
  const content = (selectedWin: Win) => (
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: role === 'user' ? 'alice' : 'operator',
            role,
            tenant_scope: null,
            token_prefix: null,
            self_user: role === 'user' ? { tenant_id: tenant, user_id: 'alice' } : undefined,
            masked_assets: role === 'user',
          },
          initial: { node_count: 0, chain_group_count: [] },
        }}
      >
        <UsersPane win={selectedWin} />
      </SessionProvider>
    </QueryClientProvider>
  );
  const view = render(content(win));
  const selectUser = (id: string) => view.rerender(content({ ...win, data: { drill: { p: 'user', id } } }));
  const row = (id: string) => {
    const found = [...view.container.querySelectorAll<HTMLElement>('.user-row')].find(
      candidate => candidate.querySelector('.user-row-name b')?.textContent === id,
    );
    if (!found) throw new Error(`missing roster row ${id}`);
    return found;
  };
  return { view, row, selectUser, client };
}

describe('user roster rows', () => {
  it.each([
    [0, '余裕'],
    [49.99, '余裕'],
    [50, '余裕'],
    [74.99, '余裕'],
    [75, '余裕'],
    [94.99, '余裕'],
    [95, '95%'],
    [99.99, '100%'],
    [100.82, '101%'],
  ])('maps quota usage %s to roster stage %s', (pct, stage) => {
    expect(quotaStage(pct)).toBe(stage);
  });

  it.each([false, true])('navigates to the top when selecting a user (narrow=%s)', narrow => {
    vi.stubGlobal('matchMedia', () => ({ matches: narrow, addEventListener() {}, removeEventListener() {} }));
    const navigate = vi.spyOn(route, 'navigate').mockReturnValue(true);
    const navigateInPlace = vi.spyOn(route, 'navigateInPlace').mockReturnValue(true);
    const { row } = mount();

    fireEvent.click(row('dave'));
    expect(navigate).toHaveBeenCalledExactlyOnceWith('users', { p: 'user', id: 'dave' });
    expect(navigateInPlace).not.toHaveBeenCalled();
  });

  it('switches details immediately while preserving the roster and isolating per-user editors', () => {
    const { view, selectUser } = mount();
    const roster = view.getByRole('listbox', { name: '用户列表' });
    const aliceDetail = view.container.querySelector('.user-split-detail');
    fireEvent.click(view.getByRole('button', { name: '改额度' }));
    expect(view.getByPlaceholderText('留空 = 不限').getAttribute('value')).toBe('100');

    selectUser('dave');
    const daveDetail = view.container.querySelector('.user-split-detail');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(daveDetail).not.toBe(aliceDetail);
    expect(daveDetail?.querySelector('.dname > b')?.textContent).toBe('dave');
    expect(view.queryByPlaceholderText('留空 = 不限')).toBeNull();
    expect(view.queryByText('加载中…')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '改额度' }));
    expect(view.getByPlaceholderText('留空 = 不限').getAttribute('value')).toBe('50');

    selectUser('alice');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(view.container.querySelector('.user-split-detail .dname > b')?.textContent).toBe('alice');
    expect(view.queryByPlaceholderText('留空 = 不限')).toBeNull();
  });

  it('shows online sources and quota for a normal user without a status lamp', () => {
    const { view, row } = mount();
    const alice = row('alice');
    expect(alice.querySelector('.node-lamp')).toBeNull();
    expect(alice.querySelector('.user-row-presence.online')?.textContent).toBe('在线来源 1');
    expect(alice.querySelector('.geo-flag')).toBeNull();
    expect(alice.querySelector('.user-row-usage')?.textContent).toBe('54.1GiB');
    expect(alice.querySelector('.user-row-quota')?.textContent).toBe('余裕');
    expect(alice.querySelector('.user-row-quota')?.getAttribute('title')).toBe('额度 余裕');
    expect(alice.querySelectorAll('.user-quota-ring circle')).toHaveLength(2);
    expect(alice.querySelector('.user-quota-ring.over')).toBeNull();
    expect(view.container.querySelector('.user-dcard .qta-meta > span')?.textContent).toBe('54%');
    expect(alice.querySelector('.user-row-chip')).toBeNull();
    expect(view.getByText('1.1.1.1')).toBeTruthy();
    expect(view.getByRole('button', { name: '复制来源 IP 1.1.1.1' })).toBeTruthy();
  });

  it('loads historical source IPs only after an administrator expands them', async () => {
    const historyPath = `/users/${tenant}/alice/presence-history`;
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/grant-probes/capability') {
        return Response.json({ available: false, reason: '测试中不发起拨测' });
      }
      if (path === historyPath) {
        return Response.json({
          tenant_id: tenant,
          user_id: 'alice',
          retention_days: 30,
          truncated: false,
          source_countries: [{ ip: '8.8.8.8', country: 'JP' }],
          source_operators: [{ ip: '8.8.8.8', operator: 'cernet' }],
          sources: [
            {
              ip: '8.8.8.8',
              first_observed_at: '2026-09-28T00:00:00Z',
              last_observed_at: '2026-09-29T00:00:00Z',
              xray_last_seen_at: '2026-09-28T23:59:58Z',
              node_ids: ['n1'],
              ingress_ids: ['tyo-iij'],
              accesses: [{ node_id: 'n1', ingress_id: 'tyo-iij', protocols: ['hysteria2'] }],
            },
          ],
        });
      }
      throw new Error(`unexpected ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const { view } = mount();
    const toggle = view.getByRole('button', { name: '历史来源 IP' });

    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(view.queryByText('8.8.8.8')).toBeNull();
    expect(fetchMock.mock.calls.some(([input]) => String(input) === historyPath)).toBe(false);

    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    expect(await view.findByText('8.8.8.8')).toBeTruthy();
    expect(view.getByText('最近 30 天 · 1 个已离线来源')).toBeTruthy();
    const historyRow = view.getByText('8.8.8.8').closest('.user-presence-source')!;
    expect(within(historyRow as HTMLElement).getByText('日本 · 教育网')).toBeTruthy();
    expect(within(historyRow as HTMLElement).getByText('曾接入')).toBeTruthy();
    expect(within(historyRow as HTMLElement).getByRole('button', { name: '东京测试节点' })).toBeTruthy();
    expect(within(historyRow as HTMLElement).queryByText('Hysteria2')).toBeNull();
    fireEvent.click(within(historyRow as HTMLElement).getByRole('button', { name: '8.8.8.8 的最后观测协议' }));
    expect(within(historyRow as HTMLElement).getByText('最后观测协议')).toBeTruthy();
    expect(within(historyRow as HTMLElement).getByText('Hysteria2')).toBeTruthy();
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === historyPath)).toHaveLength(1);

    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(view.queryByText('8.8.8.8')).toBeNull();
  });

  it('shows only access point counts to a signed-in user and never requests presence', () => {
    const { view, row } = mount('user');
    const alice = row('alice');

    expect(alice.querySelector('.user-row-presence')?.textContent).toBe('1 个接入点');
    expect(view.queryByText('在线来源')).toBeNull();
    expect(view.queryByText('在线接入')).toBeNull();
    expect(view.queryByText('台湾')).toBeNull();
    expect(view.queryByText('1.1.1.1')).toBeNull();
    expect(view.queryByRole('button', { name: '历史来源 IP' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/presence'))).toBe(false);
  });

  it('shows local source location and node names with working node navigation', () => {
    const navigate = vi.spyOn(route, 'navigate').mockReturnValue(true);
    const { view } = mount();
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(within(card).getByText('在线接入')).toBeTruthy();
    expect(within(card).getByText('1 个公网来源 · 覆盖 1 台节点')).toBeTruthy();
    expect(within(card).getByText('台湾')).toBeTruthy();
    expect(within(card).getByRole('img', { name: 'TW 地区旗' })).toBeTruthy();
    fireEvent.click(within(card).getByRole('button', { name: '东京测试节点' }));
    expect(navigate).toHaveBeenCalledExactlyOnceWith('nodes', { p: 'node', id: 'n1' });
    expect(view.getByLabelText('已授权 1 个接入点')).toBeTruthy();
    expect(vi.mocked(fetch).mock.calls.every(([input]) => String(input) === '/grant-probes/capability')).toBe(true);
  });

  it('counts shared source IPs once globally and once per node, including IPv6', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    const source = data.users[0].sources[0];
    act(() =>
      client.setQueryData(key, {
        ...data,
        source_countries: [{ ip: '2001:db8::1', country: 'MO' }],
        users: [
          {
            ...data.users[0],
            expected_nodes: 5,
            reporting_nodes: 5,
            sources: [
              { ...source, node_ids: ['n1', 'n2'], ingress_ids: ['i1', 'i2'] },
              { ...source, ip: '2001:db8::1', node_ids: ['n2'], ingress_ids: ['i2'] },
            ],
          },
        ],
      }),
    );
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(await within(card).findByText('2 个公网来源 · 覆盖 2 台节点')).toBeTruthy();
    const shared = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-source')!;
    const sharedToggle = within(shared).getByRole('button', { name: '接入 2 台节点' });
    expect(sharedToggle.getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(sharedToggle);
    expect(sharedToggle.getAttribute('aria-expanded')).toBe('true');
    expect(within(shared).getByRole('list', { name: '1.1.1.1 的接入节点' })).toBeTruthy();
    expect(within(shared).getByRole('button', { name: '东京测试节点' })).toBeTruthy();
    expect(within(shared).getByRole('button', { name: '澳门测试节点' })).toBeTruthy();
    expect(within(shared).getByText('位置未知')).toBeTruthy();
    const v6 = view.getByText('2001:db8::1').closest<HTMLElement>('.user-presence-source')!;
    expect(within(v6).getByText('澳门')).toBeTruthy();
  });

  it('reveals actual protocols only on click and keeps unknown legacy observations explicit', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    const row = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-source')!;
    const toggle = within(row).getByRole('button', { name: '1.1.1.1 的接入协议' });
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(within(row).queryByText('协议未知')).toBeNull();
    fireEvent.click(toggle);
    expect(within(row).getByText('协议未知')).toBeTruthy();
    expect(document.getElementById(toggle.getAttribute('aria-controls')!)).toBeTruthy();
    act(() =>
      client.setQueryData(key, {
        ...data,
        users: [
          {
            ...data.users[0],
            sources: [
              {
                ...data.users[0].sources[0],
                accesses: [
                  { node_id: 'n1', ingress_id: 'i1', protocols: ['vless', 'anytls'] },
                  { node_id: 'n1', ingress_id: 'i2', protocols: ['vless'] },
                  { node_id: 'n1', ingress_id: 'i3', protocols: null },
                ],
              },
            ],
          },
        ],
      }),
    );
    expect(await within(row).findByText('VLESS · AnyTLS · 协议未知')).toBeTruthy();
    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(within(row).queryByText('VLESS · AnyTLS · 协议未知')).toBeNull();
    expect(vi.mocked(fetch).mock.calls.every(([input]) => String(input) === '/grant-probes/capability')).toBe(true);
  });

  it('keeps protocols associated with their own node inside the existing multi-node disclosure', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    act(() =>
      client.setQueryData(key, {
        ...data,
        users: [
          {
            ...data.users[0],
            sources: [
              {
                ...data.users[0].sources[0],
                node_ids: ['n1', 'n2'],
                accesses: [
                  { node_id: 'n1', ingress_id: 'i1', protocols: ['vless'] },
                  { node_id: 'n2', ingress_id: 'i2', protocols: ['hysteria2', 'anytls'] },
                ],
              },
            ],
          },
        ],
      }),
    );
    const toggle = await view.findByRole('button', { name: '接入 2 台节点' });
    const row = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-source')!;
    expect(within(row).queryByText('VLESS')).toBeNull();
    fireEvent.click(toggle);
    const tokyo = within(row).getByRole('button', { name: '东京测试节点' }).closest('li')!;
    const macau = within(row).getByRole('button', { name: '澳门测试节点' }).closest('li')!;
    expect(within(tokyo).getByText('VLESS')).toBeTruthy();
    expect(within(tokyo).queryByText(/AnyTLS/)).toBeNull();
    expect(within(macau).getByText('AnyTLS · Hysteria2')).toBeTruthy();
    expect(within(macau).queryByText('VLESS')).toBeNull();
    expect(view.getByText('1 个公网来源 · 覆盖 2 台节点')).toBeTruthy();
  });

  it.each([
    ['chinanet', '电信'],
    ['cmcc', '移动'],
    ['unicom', '联通'],
    ['cernet', '教育网'],
    ['cstnet', '科技网'],
  ])('shows local %s attribution beside the country without an extra request', async (operator, label) => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    act(() =>
      client.setQueryData(key, {
        ...data,
        source_countries: [{ ip: '2001:db8::1', country: 'CN' }],
        source_operators: [{ ip: '2001:db8::1', operator }],
        users: [{ ...data.users[0], sources: [{ ...data.users[0].sources[0], ip: '2001:db8::1' }] }],
      }),
    );
    expect(await view.findByText(`中国 · ${label}`)).toBeTruthy();
    const location = view.container.querySelector('.user-presence-location')!;
    expect(within(location as HTMLElement).getByRole('img', { name: 'CN 地区旗' })).toBeTruthy();
    expect(vi.mocked(fetch).mock.calls.every(([input]) => String(input) === '/grant-probes/capability')).toBe(true);
  });

  it.each([
    undefined,
    [],
    [{ ip: '1.1.1.1', operator: 'new-unknown-operator' }],
    [{ ip: '192.0.2.1', operator: 'cmcc' }],
  ])('keeps the country without a dangling separator when operator data is unavailable: %j', async source_operators => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    act(() =>
      client.setQueryData(key, {
        ...data,
        source_countries: [{ ip: '1.1.1.1', country: 'CN' }],
        source_operators,
      }),
    );
    expect(await view.findByText('中国')).toBeTruthy();
    expect(view.container.querySelector('.user-presence-location')?.textContent).toBe('中国');
  });

  it('does not infer the country from an operator when the country database is unavailable', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    act(() =>
      client.setQueryData(key, {
        ...data,
        source_countries: [],
        source_operators: [{ ip: '1.1.1.1', operator: 'chinanet' }],
      }),
    );
    expect(await view.findByText('位置未知 · 电信')).toBeTruthy();
    expect(view.container.querySelector('.user-presence-location .geo-flag')).toBeNull();
  });

  it.each(['partial', 'unavailable', 'complete'] as const)(
    'keeps %s snapshot coverage distinct from offline',
    async state => {
      const { view, client } = mount();
      const key = ['user-presence', 'system-admin'];
      const data = client.getQueryData<UserPresenceList>(key)!;
      act(() =>
        client.setQueryData(key, {
          ...data,
          users: [{ ...data.users[0], state, reporting_nodes: state === 'complete' ? 1 : 0, sources: [] }],
        }),
      );
      const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
      if (state === 'complete') {
        expect((await within(card).findAllByText('暂无在线连接')).length).toBeGreaterThan(0);
      } else {
        expect(within(card).queryByText('暂无在线连接')).toBeNull();
        expect((await within(card).findAllByText('在线来源 —')).length).toBeGreaterThan(0);
      }
    },
  );

  it('marks incomplete online counts as lower bounds and falls back to unknown node IDs', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    act(() =>
      client.setQueryData(key, {
        ...data,
        source_countries: undefined,
        users: [
          {
            ...data.users[0],
            state: 'partial',
            expected_nodes: 2,
            sources: [{ ...data.users[0].sources[0], node_ids: ['retired-node'] }],
          },
        ],
      }),
    );
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(await within(card).findByText('至少 1 个公网来源 · 至少覆盖 1 台节点')).toBeTruthy();
    expect(within(card).getByRole('button', { name: 'retired-node' })).toBeTruthy();
    expect(within(card).getByText('位置未知')).toBeTruthy();
  });

  it('keeps the last snapshot visible but explicitly marks a failed presence refresh', async () => {
    const { view, client } = mount();
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) === '/users/presence') throw new Error('在线状态请求失败');
        return Response.json({ available: false, reason: '测试中不发起拨测' });
      }),
    );
    await act(async () => {
      await client.refetchQueries({ queryKey: ['user-presence', 'system-admin'], exact: true });
    });
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(await within(card).findByText('在线状态暂不可用')).toBeTruthy();
    expect(within(card).getByText('当前显示上次成功读取的快照。')).toBeTruthy();
    expect(within(card).getByText('1.1.1.1')).toBeTruthy();
    expect(within(card).queryByText('暂无在线连接')).toBeNull();
  });

  it.each(['XX', 'ZZ', 'invalid-code'])('shows unknown location for an unsupported country code %s', async country => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    act(() => client.setQueryData(key, { ...data, source_countries: [{ ip: '1.1.1.1', country }] }));
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(await within(card).findByText('位置未知')).toBeTruthy();
    expect(within(card).queryByRole('img')).toBeNull();
  });

  it('marks an exhausted user with a red lamp, the affected line and an over-quota ring', () => {
    const { row } = mount();
    const dave = row('dave');
    expect(dave.querySelector('.node-lamp.bad')).toBeTruthy();
    expect(dave.querySelector('.user-row-state.bad')?.textContent).toBe('流量已用尽 · 东京');
    expect(dave.querySelector('.user-quota-ring.over')).toBeTruthy();
    expect(dave.querySelectorAll('.user-quota-ring circle')).toHaveLength(2);
    expect(dave.querySelector('.user-row-quota')?.textContent).toBe('101%');
  });

  it('labels test accounts, leaves ungranted users unlit, and groups disabled users last', () => {
    const { view, row } = mount();
    const carol = row('carol');
    expect(within(carol).getByText('测试')).toBeTruthy();
    expect(carol.querySelector('.node-lamp.idle')).toBeTruthy();
    expect(carol.querySelector('.user-row-state')?.textContent).toBe('未授权');

    const group = view.getByRole('group', { name: '已停用' });
    const erin = row('erin');
    expect(group.contains(erin)).toBe(true);
    expect(erin.classList.contains('off')).toBe(true);
    expect(erin.querySelector('.node-lamp')).toBeNull();
    expect(erin.querySelector('.user-row-state')?.textContent).toBe('已停用');
  });

  it('filters the roster with counts per filter', () => {
    const { view } = mount();
    const filter = view.getByRole('group', { name: '筛选用户' });
    expect([...filter.querySelectorAll('button')].map(button => button.textContent)).toEqual([
      '全部 4',
      '需处理 1',
      '测试 1',
      '已停用 1',
    ]);
    expect(filter.querySelector('i.attention')?.textContent).toBe('1');

    fireEvent.click(within(filter).getByRole('button', { name: '需处理 1' }));
    const names = [...view.container.querySelectorAll('.user-row .user-row-name b')].map(name => name.textContent);
    expect(names).toEqual(['dave']);
  });

  it('offers a direct-login page action for each user', async () => {
    const pending = new Promise<Response>(() => {});
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/grant-probes/capability') {
        return Promise.resolve(Response.json({ available: false, reason: '测试中不发起拨测' }));
      }
      if (path === `/users/${tenant}/alice/direct-login`) return pending;
      return Promise.reject(new Error(`unexpected ${path}`));
    });
    vi.stubGlobal('fetch', fetchMock);
    const { view } = mount();

    fireEvent.click(view.getByRole('button', { name: '更多操作' }));
    fireEvent.click(view.getByRole('menuitem', { name: /生成直达登录页/ }));

    await vi.waitFor(() =>
      expect(fetchMock.mock.calls.some(([input]) => String(input) === `/users/${tenant}/alice/direct-login`)).toBe(
        true,
      ),
    );
  });
});
