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
      // 详情打开即读取离线来源；默认没有离线记录
      const history = String(input).match(/^\/users\/([^/]+)\/([^/]+)\/presence-history$/);
      if (history) {
        return Response.json({
          tenant_id: history[1],
          user_id: history[2],
          retention_days: 30,
          truncated: false,
          sources: [],
        });
      }
      throw new Error(`unexpected ${String(input)}`);
    }),
  );
});

// 详情打开后的后台读取只有拨测能力和当前用户的离线来源，其余请求都来自显式操作。
const backgroundReadsOnly = () =>
  vi
    .mocked(fetch)
    .mock.calls.every(
      ([input]) => String(input) === '/grant-probes/capability' || String(input).endsWith('/presence-history'),
    );
const readingOf = (card: HTMLElement) => card.querySelector('header .rt')?.textContent;
const two = (value: number) => String(value).padStart(2, '0');
const HOUR = 3_600_000;
const offlineSource = (ip: string, lastSeen: number, protocols: string[]) => ({
  ip,
  first_observed_at: new Date(lastSeen - HOUR).toISOString(),
  last_observed_at: new Date(lastSeen).toISOString(),
  xray_last_seen_at: new Date(lastSeen - 60_000).toISOString(),
  node_ids: ['n1'],
  ingress_ids: ['tyo-iij'],
  accesses: [{ node_id: 'n1', ingress_id: 'tyo-iij', protocols }],
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
    const quotaInput = () => view.queryByRole('textbox', { name: '东京 的月度额度（GiB）' }) as HTMLInputElement | null;
    const roster = view.getByRole('listbox', { name: '用户列表' });
    const aliceDetail = view.container.querySelector('.user-split-detail');
    fireEvent.click(view.getByRole('button', { name: '改额度' }));
    expect(quotaInput()?.value).toBe('100');

    selectUser('dave');
    const daveDetail = view.container.querySelector('.user-split-detail');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(daveDetail).not.toBe(aliceDetail);
    expect(daveDetail?.querySelector('.dname > b')?.textContent).toBe('dave');
    expect(quotaInput()).toBeNull();
    expect(view.queryByText('加载中…')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '改额度' }));
    expect(quotaInput()?.value).toBe('50');

    selectUser('alice');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(view.container.querySelector('.user-split-detail .dname > b')?.textContent).toBe('alice');
    expect(quotaInput()).toBeNull();
  });

  it('reads the month and every route of the selected user in one usage card', () => {
    const { view, selectUser } = mount();
    const card = () => view.container.querySelector<HTMLElement>('.user-usage-card')!;
    expect(card().querySelector('header .rt')?.textContent).toBe('1 条线路');
    expect(card().querySelector('.user-usage-hero-value')?.textContent).toBe('54.10 GiB');
    expect([...card().querySelectorAll('.usage-io dd:not(.usage-share)')].map(cell => cell.textContent)).toEqual([
      '0 B',
      '54.10 GiB',
    ]);
    const tokyo = card().querySelector<HTMLElement>('.qta-r')!;
    expect(tokyo.classList.contains('ok')).toBe(true);
    expect(tokyo.querySelector('.qta-app')?.textContent).toBe('东京');
    expect(tokyo.querySelectorAll('.qta-app .geo-flag')).toHaveLength(1);
    // 线路名在前，地区旗跟在名字后面。
    expect([...tokyo.querySelector('.qta-app')!.children].map(child => child.tagName.toLowerCase())).toEqual([
      'b',
      'span',
    ]);
    expect(tokyo.querySelector('.qta-app > :last-child')?.classList.contains('qta-flags')).toBe(true);
    expect(tokyo.querySelector('.qta-sub')?.textContent).toBe('东京 IIJ');
    expect(tokyo.querySelector('.qta-figs')?.textContent).toBe('54.10GiB/ 100 GiB');
    expect(tokyo.querySelector('.qta-left')?.textContent).toBe('剩余45.90 GiB');
    expect(card().querySelector('.user-dcard-foot')).toBeNull();

    // dave 用尽额度：执行器撤销的接入点不在 grants 里，只能从额度记录读出。
    selectUser('dave');
    const daveRoute = card().querySelector<HTMLElement>('.qta-r')!;
    expect(daveRoute.classList.contains('over')).toBe(true);
    expect(daveRoute.querySelector('.qta-sub.stop')?.textContent).toBe('系统已停用 1 个接入点');
    expect(daveRoute.querySelector('.qta-sub.stop')?.getAttribute('title')).toBe(
      '东京 IIJ 已被系统停用；补足额度或月初重置后自动恢复',
    );
    expect(daveRoute.querySelector('.qta-left')?.textContent).toBe('超出419.8 MiB');
    expect(daveRoute.querySelector('.qta-pct')?.textContent).toBe('101%');
  });

  it('shows online sources and quota for a normal user without a status lamp', () => {
    const { view, row } = mount();
    const alice = row('alice');
    expect(alice.querySelector('.node-lamp')).toBeNull();
    expect(alice.querySelector('.user-row-presence.online')?.textContent).toBe('在线来源 1');
    expect(alice.querySelector('.geo-flag')).toBeNull();
    expect(alice.querySelector('.user-row-usage')?.textContent).toBe('54.1GiB');
    expect(alice.querySelector('.user-row-quota')?.textContent).toBe('余裕');
    expect(alice.querySelector('.user-row-quota')?.getAttribute('title')).toBe('总额度 余裕');
    expect(alice.querySelectorAll('.user-quota-ring circle')).toHaveLength(2);
    expect(alice.querySelector('.user-quota-ring.over')).toBeNull();
    expect(view.container.querySelector('.user-dcard .qta-pct')?.textContent).toBe('54%');
    expect(alice.querySelector('.user-row-chip')).toBeNull();
    expect(view.getByText('1.1.1.1')).toBeTruthy();
    expect(view.getByRole('button', { name: '复制来源 IP 1.1.1.1' })).toBeTruthy();
  });

  it('reads offline sources with the detail and groups them by the day they were last seen', async () => {
    const historyPath = `/users/${tenant}/alice/presence-history`;
    // 以本地当天零点为基准，分组不随测试运行的时刻变化
    const today = new Date().setHours(0, 0, 0, 0);
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
            offlineSource('8.8.8.8', today + 60_000, ['hysteria2']),
            offlineSource('9.9.9.9', today - 12 * HOUR, ['vless']),
            offlineSource('2001:db8::5', today - 60 * HOUR, ['anytls']),
            offlineSource('4.4.4.4', today - 20 * 24 * HOUR, ['vless']),
          ],
        });
      }
      throw new Error(`unexpected ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const { view } = mount();
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;

    expect(await within(card).findByText('8.8.8.8')).toBeTruthy();
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === historyPath)).toHaveLength(1);
    expect(readingOf(card)).toBe('在线 1 · 30 天 5 个地址');
    expect([...card.querySelectorAll('.user-presence-day')].map(day => day.textContent)).toEqual([
      '在线',
      '今天',
      '昨天',
    ]);
    const recent = view.getByText('8.8.8.8').closest<HTMLElement>('.user-presence-row')!;
    expect(recent.classList.contains('off')).toBe(true);
    expect(recent.querySelector('.user-presence-time')?.textContent).toBe('00:01');
    expect(within(recent).getByText('日本 · 教育网')).toBeTruthy();
    const accessToggle = within(recent).getByRole('button', { name: '展开 1 个入口详情' });
    expect(accessToggle.getAttribute('aria-expanded')).toBe('false');
    expect(within(recent).queryByRole('button', { name: '东京测试节点' })).toBeNull();
    fireEvent.click(accessToggle);
    expect(accessToggle.getAttribute('aria-expanded')).toBe('true');
    expect(within(recent).getByRole('button', { name: '东京测试节点' })).toBeTruthy();
    expect(within(recent).getByText('Hysteria2')).toBeTruthy();
    expect(within(recent).getByRole('button', { name: '复制历史来源 IP 8.8.8.8' })).toBeTruthy();
    expect(within(card).getByText('9.9.9.9')).toBeTruthy();
    expect(within(card).queryByText('2001:db8::5')).toBeNull();
    expect(within(card).queryByText('4.4.4.4')).toBeNull();

    const older = within(card).getByRole('button', { name: '显示更早的 2 个来源' });
    expect(older.getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(older);
    expect([...card.querySelectorAll('.user-presence-day')].map(day => day.textContent)).toEqual([
      '在线',
      '今天',
      '昨天',
      '7 天内',
      '30 天内',
    ]);
    expect(within(card).getByText('2001:db8::5')).toBeTruthy();
    const oldest = within(card).getByText('4.4.4.4').closest<HTMLElement>('.user-presence-row')!;
    const oldestDay = new Date(today - 20 * 24 * HOUR);
    expect(oldest.querySelector('.user-presence-time')?.textContent).toBe(
      `${two(oldestDay.getMonth() + 1)}-${two(oldestDay.getDate())}`,
    );
    fireEvent.click(within(card).getByRole('button', { name: '收起更早的来源' }));
    expect(within(card).queryByText('4.4.4.4')).toBeNull();
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === historyPath)).toHaveLength(1);
  });

  it('lists a returning address only once and summarizes an offline user by the last appearance', async () => {
    const historyPath = `/users/${tenant}/alice/presence-history`;
    const today = new Date().setHours(0, 0, 0, 0);
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
          sources: [
            offlineSource('1.1.1.1', today + 60_000, ['vless']),
            offlineSource('8.8.8.8', today - 12 * HOUR, ['vless']),
          ],
        });
      }
      throw new Error(`unexpected ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const { view, client } = mount();
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;

    expect(await within(card).findByText('8.8.8.8')).toBeTruthy();
    expect(within(card).getAllByText('1.1.1.1')).toHaveLength(1);
    expect(within(card).getByText('1.1.1.1').closest('.user-presence-row')?.classList.contains('off')).toBe(false);
    expect(readingOf(card)).toBe('在线 1 · 30 天 2 个地址');

    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    act(() => client.setQueryData(key, { ...data, users: [{ ...data.users[0], sources: [] }] }));
    await vi.waitFor(() => expect(readingOf(card)).toMatch(/^离线 · 最后出现 /));
    expect(within(card).getByText('暂无在线连接')).toBeTruthy();
    expect(within(card).getByText('1.1.1.1').closest('.user-presence-row')?.classList.contains('off')).toBe(true);
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === historyPath)).toHaveLength(2);
  });

  it('keeps a full IPv6 address copyable while dimming its interface identifier', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    const ip = '2408:8207:2464:1a50:4c3b:9f2e:11d0:7a21';
    act(() =>
      client.setQueryData(key, {
        ...data,
        users: [{ ...data.users[0], sources: [{ ...data.users[0].sources[0], ip }] }],
      }),
    );
    const copy = await view.findByRole('button', { name: `复制来源 IP ${ip}` });
    const row = copy.closest<HTMLElement>('.user-presence-row')!;
    expect(row.querySelector('.user-presence-ip > code')?.textContent).toBe(ip);
    expect(row.querySelector('.user-presence-iid')?.textContent).toBe(':4c3b:9f2e:11d0:7a21');
    expect(row.querySelectorAll('.user-presence-iid wbr')).toHaveLength(4);
  });

  it('shows only access point counts to a signed-in user and never requests presence', () => {
    const { view, row } = mount('user');
    const alice = row('alice');

    expect(alice.querySelector('.user-row-presence')?.textContent).toBe('1 个接入点');
    expect(view.queryByText('在线来源')).toBeNull();
    expect(view.queryByText('在线接入')).toBeNull();
    expect(view.queryByText('台湾')).toBeNull();
    expect(view.queryByText('1.1.1.1')).toBeNull();
    expect(view.container.querySelector('.user-presence-card')).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/presence'))).toBe(false);
  });

  it('shows local source location and node names with working node navigation', async () => {
    const navigate = vi.spyOn(route, 'navigate').mockReturnValue(true);
    const { view } = mount();
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(within(card).getByText('在线接入')).toBeTruthy();
    await vi.waitFor(() => expect(readingOf(card)).toBe('在线 1 · 30 天 1 个地址'));
    expect(within(card).getByText('台湾')).toBeTruthy();
    expect(within(card).getByRole('img', { name: 'TW 地区旗' })).toBeTruthy();
    expect(within(card).getByText('1 个入口 · 协议未知')).toBeTruthy();
    fireEvent.click(within(card).getByRole('button', { name: '展开 1 个入口详情' }));
    fireEvent.click(within(card).getByRole('button', { name: '东京测试节点' }));
    expect(navigate).toHaveBeenCalledExactlyOnceWith('nodes', { p: 'node', id: 'n1' });
    expect(view.getByLabelText('已授权 1 个接入点')).toBeTruthy();
    expect(backgroundReadsOnly()).toBe(true);
  });

  it('summarizes shared source entry points and expands every node without recounting the address', async () => {
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
    await vi.waitFor(() => expect(readingOf(card)).toBe('在线 2 · 30 天 2 个地址'));
    const shared = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-row')!;
    expect(within(shared).getByText('2 个入口 · 协议未知')).toBeTruthy();
    expect(within(shared).queryByRole('button', { name: '东京测试节点' })).toBeNull();
    const accessToggle = within(shared).getByRole('button', { name: '展开 2 个入口详情' });
    fireEvent.click(accessToggle);
    expect(accessToggle.getAttribute('aria-expanded')).toBe('true');
    expect(within(shared).getByRole('button', { name: '东京测试节点' })).toBeTruthy();
    expect(within(shared).getByRole('button', { name: '澳门测试节点' })).toBeTruthy();
    expect(within(shared).getAllByText('协议未知')).toHaveLength(2);
    expect(within(shared).getByText('位置未知')).toBeTruthy();
    fireEvent.click(within(shared).getByRole('button', { name: '收起 2 个入口详情' }));
    expect(accessToggle.getAttribute('aria-expanded')).toBe('false');
    expect(within(shared).queryByRole('button', { name: '东京测试节点' })).toBeNull();
    const v6 = view.getByText('2001:db8::1').closest<HTMLElement>('.user-presence-row')!;
    expect(within(v6).getByText('澳门')).toBeTruthy();
  });

  it('shows observed protocols in expanded details and keeps unknown legacy observations explicit', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    const row = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-row')!;
    expect(within(row).getByText('1 个入口 · 协议未知')).toBeTruthy();
    fireEvent.click(within(row).getByRole('button', { name: '展开 1 个入口详情' }));
    expect(within(row).getByText('协议未知')).toBeTruthy();
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
    expect(backgroundReadsOnly()).toBe(true);
  });

  it('keeps protocols associated with their own node', async () => {
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
    const row = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-row')!;
    fireEvent.click(await within(row).findByRole('button', { name: '展开 2 个入口详情' }));
    const tokyo = (await within(row).findByRole('button', { name: '东京测试节点' })).closest<HTMLElement>(
      '.user-presence-access-item',
    )!;
    const macau = within(row)
      .getByRole('button', { name: '澳门测试节点' })
      .closest<HTMLElement>('.user-presence-access-item')!;
    expect(within(tokyo).getByText('VLESS')).toBeTruthy();
    expect(within(tokyo).queryByText(/AnyTLS/)).toBeNull();
    expect(within(macau).getByText('AnyTLS · Hysteria2')).toBeTruthy();
    expect(within(macau).queryByText('VLESS')).toBeNull();
  });

  it.each([
    ['chinanet', '电信'],
    ['cmcc', '移动'],
    ['unicom', '联通'],
    ['cernet', '教育网'],
    ['cstnet', '科技网'],
  ])('names the domestic %s network without an extra request', async (operator, label) => {
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
    expect(await view.findByText(`中国${label}`)).toBeTruthy();
    const location = view.container.querySelector('.user-presence-location')!;
    expect(within(location as HTMLElement).getByRole('img', { name: 'CN 地区旗' })).toBeTruthy();
    expect(backgroundReadsOnly()).toBe(true);
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
        expect(await within(card).findByText('最近 30 天没有来源记录')).toBeTruthy();
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
    await vi.waitFor(() => expect(readingOf(card)).toBe('在线至少 1 · 30 天 1 个地址'));
    expect(within(card).getByText('当前仅收到 1 / 2 台入口节点的最新快照，在线来源可能不完整')).toBeTruthy();
    fireEvent.click(within(card).getByRole('button', { name: '展开 1 个入口详情' }));
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
