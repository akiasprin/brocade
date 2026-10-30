import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  AdminRole,
  ConsoleSnapshot,
  OnlineSourceProtocol,
  SnapshotIngress,
  SourceAccessView,
  UserListItem,
  UserPresenceList,
} from '../src/api';
import { draft } from '../src/draft';
import * as route from '../src/forge/route';
import { mergePresenceHistory, quotaStage, sessionDurationText, UsersPane } from '../src/panes/users';
import { userTrafficPolicyKey } from '../src/panes/user-traffic-policy';
import { SessionProvider } from '../src/session';
import { Icon } from '../src/ui/icons';
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
        chains: [
          { id: 'jp-iij', name: '东京 IIJ', subscription_country: 'JP' },
          { id: 'mo-cmi', name: '澳门 CMI', subscription_country: 'MO' },
        ],
        steps: [],
        fronts: [],
        ingresses: [
          { id: 'tyo-iij', chain: 'jp-iij', node: 'n1', port: 443 } as SnapshotIngress,
          { id: 'mfm-cmi', chain: 'mo-cmi', node: 'n2', port: 443 } as SnapshotIngress,
        ],
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
const isForwardingRead = (path: string) => /^\/users\/[^/]+\/[^/]+\/forwarding$/.test(path);
const forwardingEmpty = (path: string) => {
  const [, , tenantId, userId] = path.split('/');
  return { tenant_id: tenantId, user_id: userId, entitlements: [], rules: [] };
};
// 订阅地址是访问凭据：只在详情标题栏打开订阅与节点弹窗的「订阅」页签时读取（管理员读用户的，用户本人读 /me 的）。
const isSubscriptionRead = (path: string) =>
  /^\/users\/[^/]+\/[^/]+\/clash-subscription$/.test(path) || path === '/me/clash-subscription';
const subscriptionUrl = 'https://sub.example/sub/v1/2d2304da-f114-4574-8d44-625afdb1db5c/clash.yaml';
const subscription = {
  url: subscriptionUrl,
  urls: { both: subscriptionUrl, v4: `${subscriptionUrl}?family=v4`, v6: `${subscriptionUrl}?family=v6` },
  template: 'SubBoost 标准版',
  haitun: null,
  remaining_bytes: null,
  reset_at: '2026-10-01T00:00:00+08:00',
  usage_has_gap: false,
};
beforeEach(() => {
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      if (String(input).endsWith('/source-access')) return Response.json({ revision: 0, ips: [], nodes: [] });
      if (String(input) === '/grant-probes/capability') {
        return Response.json({ available: false, reason: '测试中不发起拨测' });
      }
      if (String(input).endsWith('/traffic-policy'))
        return Response.json(
          ['access', 'forwarding'].map(kind => ({
            kind,
            app_id: kind === 'access' ? 'tokyo' : null,
            policy: null,
            used_bytes: '0',
            exhausted: false,
            service_available: true,
            unsupported_nodes: [],
          })),
        );
      if (isForwardingRead(String(input))) return Response.json(forwardingEmpty(String(input)));
      if (isSubscriptionRead(String(input))) return Response.json(subscription);
      if (String(input).endsWith('/realtime/snapshot')) return Response.json({ policy: { enabled: true }, nodes: [] });
      if (String(input).endsWith('/realtime/history')) {
        return Response.json({ range_start_unix_secs: 0, range_end_unix_secs: 3_600, nodes: [], series: [] });
      }
      if (String(input).includes('/machine-usage?'))
        return Response.json({
          period: 'day',
          range_start_unix_secs: 0,
          range_end_unix_secs: 3600,
          period_end_unix_secs: 86400,
          bucket_secs: 3600,
          reporting_nodes: 0,
          expected_nodes: 1,
          rows: [],
        });
      if (String(input).includes('/site-usage?'))
        return Response.json({
          collection_enabled: true,
          catalog_version: 1,
          period: 'day',
          range_start_unix_secs: 0,
          range_end_unix_secs: 3600,
          period_end_unix_secs: 86_400,
          bucket_secs: 3600,
          series: [],
          reporting_nodes: 0,
          expected_nodes: 1,
          limited: false,
          latest_report_unix_secs: null,
          observed_since_unix_secs: null,
          rows: [],
        });
      if (/\/(other-domains|sniffing-failed-targets)\?/.test(String(input)))
        return Response.json({
          collection_enabled: false,
          top_n: 128,
          range_start_unix_secs: 0,
          range_end_unix_secs: 3600,
          reporting_nodes: 0,
          expected_nodes: 1,
          limited: false,
          latest_report_unix_secs: null,
          observed_since_unix_secs: null,
          rows: [],
        });
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

// 详情打开后的后台读取只有转发授权、当前用户的离线来源、站点统计与实时流量（快照和近 1 小时），
// 其余请求都来自显式操作：订阅地址、节点链接与拨测都在弹窗打开后才读取。
const backgroundReadsOnly = () =>
  vi
    .mocked(fetch)
    .mock.calls.every(
      ([input]) =>
        String(input).endsWith('/source-access') ||
        String(input).endsWith('/traffic-policy') ||
        String(input).endsWith('/presence-history') ||
        String(input).endsWith('/realtime/snapshot') ||
        String(input).endsWith('/realtime/history') ||
        String(input).includes('/site-usage?') ||
        String(input).includes('/machine-usage?') ||
        String(input).includes('/other-domains?') ||
        String(input).includes('/sniffing-failed-targets?') ||
        isForwardingRead(String(input)),
    );
// 查询缓存的写入在 React Query 的下一个调度点才通知观察者：在 act 内把这一拍走完再交互。
const seedUsers = async (client: QueryClient, list: UserListItem[]) => {
  await act(async () => {
    client.setQueryData(['users'], { users: list });
    await new Promise(resolve => setTimeout(resolve, 0));
  });
};
const readingOf = (card: HTMLElement) => card.querySelector('header .rt')?.textContent;
const two = (value: number) => String(value).padStart(2, '0');
const HOUR = 3_600_000;
const offlineSource = (ip: string, lastSeen: number, protocols: OnlineSourceProtocol[]) => ({
  ip,
  first_observed_at: new Date(lastSeen - HOUR).toISOString(),
  last_observed_at: new Date(lastSeen).toISOString(),
  xray_last_seen_at: new Date(lastSeen - 60_000).toISOString(),
  online_since_at: new Date(lastSeen - HOUR).toISOString(),
  session_uplink_bytes: 1_024,
  session_downlink_bytes: 2_048,
  node_ids: ['n1'],
  ingress_ids: ['tyo-iij'],
  accesses: [
    {
      node_id: 'n1',
      ingress_id: 'tyo-iij',
      protocols,
      last_observed_at: new Date(lastSeen).toISOString(),
      xray_last_seen_at: new Date(lastSeen - 60_000).toISOString(),
      online_since_at: new Date(lastSeen - HOUR).toISOString(),
      session_uplink_bytes: 1_024,
      session_downlink_bytes: 2_048,
    },
  ],
});
afterEach(() => {
  cleanup();
  clients.forEach(client => client.clear());
  clients.length = 0;
  draft.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('presence history merging', () => {
  it('limits the entire group to strictly less than an hour, regardless of input order or intervening IPs', () => {
    const at = Date.parse('2026-10-28T00:15:00Z');
    const source = (minutes: number, ip = '198.51.100.7') => offlineSource(ip, at - minutes * 60_000, ['vless']);
    const sources = [source(90), source(30), source(0), source(10, '203.0.113.8'), source(60), source(59)];
    const original = structuredClone(sources);
    const rows = mergePresenceHistory(sources, false);
    expect(rows.map(row => [row.ip, row.history?.length ?? 1])).toEqual([
      ['198.51.100.7', 3],
      ['203.0.113.8', 1],
      ['198.51.100.7', 2],
    ]);
    expect(rows[0].history?.map(item => item.last_observed_at)).toEqual(
      [source(0), source(30), source(59)].map(item => item.last_observed_at),
    );
    expect(rows[0].session_downlink_bytes).toBe(6_144);
    expect(sources).toEqual(original);
  });

  it('keeps unknown metrics unknown and does not merge unparseable timestamps or masked addresses', () => {
    const at = Date.parse('2026-10-28T00:15:00Z');
    const newest = offlineSource('198.51.100.7', at, ['vless']);
    const missing = { ...offlineSource(newest.ip, at - 60_000, ['anytls']), session_downlink_bytes: null };
    const invalid = { ...newest, last_observed_at: 'unknown' };
    const merged = mergePresenceHistory([missing, newest, invalid], false);
    expect(merged).toHaveLength(2);
    expect(merged[0].session_downlink_bytes).toBeNull();
    expect(merged[0].session_uplink_bytes).toBe(2_048);
    expect(merged[1].last_observed_at).toBe('unknown');
    expect(mergePresenceHistory([newest, missing], true)).toEqual([newest, missing]);
  });
});

function mount(
  role: AdminRole = 'system-admin',
  operatorId = role === 'user' ? 'alice' : 'operator',
  masked = role === 'user',
  seedPolicies = true,
  seedCycle = true,
) {
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
            online_since_at: '2026-09-28T23:01:00Z',
            session_uplink_bytes: 1_024,
            session_downlink_bytes: 2_048,
            node_ids: ['n1'],
            ingress_ids: ['tyo-iij'],
            accesses: [
              {
                node_id: 'n1',
                ingress_id: 'tyo-iij',
                protocols: null,
                last_observed_at: '2026-09-29T00:01:00Z',
                xray_last_seen_at: '2026-09-29T00:00:58Z',
                online_since_at: '2026-09-28T23:01:00Z',
                session_uplink_bytes: 1_024,
                session_downlink_bytes: 2_048,
              },
            ],
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
  client.setQueryData(['user-presence', 'readonly'], client.getQueryData(['user-presence', 'system-admin']));
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
  const limits = new Map([
    ['alice', 100],
    ['dave', 50],
  ]);
  for (const user of seedPolicies ? users : [])
    client.setQueryData(
      userTrafficPolicyKey(user),
      ['access', 'forwarding'].map(kind => ({
        kind,
        app_id: kind === 'access' ? 'tokyo' : null,
        policy:
          kind === 'access' && limits.has(user.id)
            ? { quota_bytes: String(limits.get(user.id)! * GiB), exhausted_action: { kind: 'suspend' } }
            : null,
        used_bytes: '0',
        exhausted: user.id === 'dave',
        service_available: true,
        unsupported_nodes: [],
        pending_nodes: [],
      })),
    );
  if (seedCycle)
    client.setQueryData(['user-cycles'], { views: [usage('alice', 54.1), usage('dave', 50.41), usage('erin', 12.3)] });
  const content = (selectedWin: Win) => (
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: operatorId,
            role,
            tenant_scope: null,
            token_prefix: null,
            self_user: role === 'user' ? { tenant_id: tenant, user_id: 'alice' } : undefined,
            masked_assets: masked,
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
  it('保留账期编辑输入并在原位展示后台名册刷新错误', async () => {
    const { view, client } = mount();
    const terms = view.getByRole('button', { name: /^重置日/ });
    expect(terms.closest('.user-usage-ledger')).toBeTruthy();
    fireEvent.click(terms);
    const input = within(view.getByRole('dialog')).getByLabelText('每月流量重置日') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '15' } });
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn((url: RequestInfo | URL, init?: RequestInit) =>
        String(url).startsWith('/users?')
          ? Promise.resolve(Response.json({ error: '名册读取失败' }, { status: 503 }))
          : original(url, init),
      ),
    );
    await act(async () => {
      await client.refetchQueries({ queryKey: ['users'], exact: true });
    });
    expect(await view.findByText('名册读取失败')).toBeTruthy();
    expect(view.getByRole('dialog')).toBeTruthy();
    expect(input.value).toBe('15');
    expect(view.getByRole('button', { name: '重新读取用户状态' })).toBeTruthy();
  });
  it('shows single/double accounting and recalculates billed monthly usage without changing raw directions', async () => {
    const originalFetch = fetch;
    const profileWrites: unknown[] = [];
    const singleDescription = '单向计量：按代理层有效流量计量，同一业务数据仅计一次。';
    const doubleDescription = '双向计量：合计服务器入站与出站流量，同一业务数据在接收和转发阶段各计一次。';
    const doubleAlice = users.map(user =>
      user.id === 'alice' ? { ...user, traffic_accounting_mode: 'double' as const } : user,
    );
    const nativeConfirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === `/users/${tenant}/alice/profile` && init?.method === 'PUT') {
          profileWrites.push(JSON.parse(String(init.body)));
          return Promise.resolve(Response.json(doubleAlice[0]));
        }
        if (path === '/users?include_disabled=true') return Promise.resolve(Response.json({ users: doubleAlice }));
        if (path === '/model/snapshot') return Promise.resolve(Response.json(snapshot));
        return originalFetch(input, init);
      }),
    );
    const { view } = mount();

    const card = view.container.querySelector<HTMLElement>('.user-usage-card')!;
    const description = within(card).getByText(singleDescription).parentElement!;
    const help = within(card).getByRole('button', { name: '计量口径说明' });
    expect(description.hidden).toBe(true);
    expect(help.getAttribute('aria-expanded')).toBe('false');
    expect(help.getAttribute('aria-controls')).toBe(description.id);
    fireEvent.click(help);
    expect(description.hidden).toBe(false);
    expect(help.getAttribute('aria-expanded')).toBe('true');
    expect(within(description).getByText(doubleDescription)).toBeTruthy();
    expect(
      within(card).getByText('切换后将按新口径重算本期用量，可能立即暂停或恢复受额度控制的接入线路或转发入口。'),
    ).toBeTruthy();
    const accountingControl = view.getByRole('group', { name: '用户流量计量口径' });
    expect(card.querySelector(':scope > header button')).toBeNull();
    expect(accountingControl.closest('.user-usage-ledger')).toBeTruthy();
    expect(accountingControl.parentElement?.previousElementSibling).toBe(card.querySelector('.user-usage-compose'));
    fireEvent.click(help);
    expect(description.hidden).toBe(true);
    fireEvent.click(within(accountingControl).getByRole('button', { name: '双向' }));
    const confirmation = view.getByRole('dialog', { name: '切换为双向计量' });
    expect(within(confirmation).getByText(/本期的接入与转发用量将按新口径重新核算/)).toBeTruthy();
    expect(within(confirmation).getByText(/重算后若额度耗尽，相关线路或转发入口将自动暂停/)).toBeTruthy();
    expect(profileWrites).toEqual([]);
    expect(within(accountingControl).getByRole('button', { name: '单向', pressed: true })).toBeTruthy();
    fireEvent.click(within(confirmation).getByRole('button', { name: '切换并重算' }));
    await vi.waitFor(() =>
      expect(profileWrites).toEqual([{ account_type: 'formal', traffic_accounting_mode: 'double' }]),
    );
    expect(nativeConfirm).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(view.queryByRole('dialog', { name: '切换为双向计量' })).toBeNull());
    expect(await within(card).findByText(doubleDescription)).toBeTruthy();
    expect(card.querySelector('.user-usage-hero-value')?.textContent).toBe('108.20 GiB');
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    const forwardingCard = view.container.querySelector<HTMLElement>('.user-forwarding-usage')!;
    const forwardingAccounting = await within(forwardingCard).findByRole('group', { name: '用户流量计量口径' });
    expect(forwardingAccounting.closest('.fwd-ledger')).toBeTruthy();
    expect(within(forwardingAccounting).getByRole('button', { name: '双向', pressed: true })).toBeTruthy();
    expect(within(forwardingCard).getByText(doubleDescription).parentElement?.hidden).toBe(true);
    fireEvent.click(within(forwardingAccounting).getByRole('button', { name: '单向' }));
    const singleConfirmation = view.getByRole('dialog', { name: '切换为单向计量' });
    expect(within(singleConfirmation).getByText(/因额度耗尽而暂停的线路或转发入口将自动恢复/)).toBeTruthy();
  });

  it.each([
    ['接入', '.user-usage-ledger'],
    ['转发', '.fwd-ledger'],
  ])('在%s读数栏的计量口径之后显示账期与有效期，并从这里打开编辑', async (tab, ledger) => {
    const { view } = mount();
    fireEvent.click(view.getByRole('tab', { name: tab }));
    const control = await view.findByRole('group', { name: '用户流量计量口径' });
    const terms = view.getByRole('button', { name: /^重置日/ });
    expect(terms.closest(ledger)).toBe(control.closest(ledger));
    const accounting = control.closest('.user-usage-accounting')!;
    expect(accounting.nextElementSibling?.classList.contains('user-usage-accounting-note')).toBe(true);
    expect(accounting.nextElementSibling?.nextElementSibling).toBe(terms);
    expect(terms.textContent).toBe('重置日每月 1 日有效期长期有效');
    fireEvent.click(terms);
    expect(view.getByRole('dialog', { name: 'alice 的账期与有效期' })).toBeTruthy();
    expect(terms.getAttribute('aria-expanded')).toBe('true');
  });

  it('只读身份的两个读数栏只显示账期与有效期', async () => {
    const { view } = mount('readonly');
    for (const tab of ['接入', '转发']) {
      fireEvent.click(view.getByRole('tab', { name: tab }));
      await view.findByRole('group', { name: '用户流量计量口径' });
      expect(view.queryByRole('button', { name: /^重置日/ })).toBeNull();
    }
    expect(view.container.querySelectorAll('dl.user-service-terms')).toHaveLength(2);
  });

  it.each(['接入', '转发'])('cancels accounting changes from %s without saving and restores focus', async tab => {
    const nativeConfirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    const { view } = mount();
    fireEvent.click(view.getByRole('tab', { name: tab }));
    const control = await view.findByRole('group', { name: '用户流量计量口径' });
    const double = within(control).getByRole('button', { name: '双向' });
    double.focus();
    fireEvent.click(double);
    fireEvent.click(within(view.getByRole('dialog', { name: '切换为双向计量' })).getByRole('button', { name: '取消' }));
    await vi.waitFor(() => expect(document.querySelector('.dialog-layer')).toBeNull());
    expect(document.activeElement).toBe(double);
    fireEvent.click(double);
    fireEvent.keyDown(document, { key: 'Escape' });
    await vi.waitFor(() => expect(document.querySelector('.dialog-layer')).toBeNull());
    expect(document.activeElement).toBe(double);
    expect(within(control).getByRole('button', { name: '单向', pressed: true })).toBeTruthy();
    expect(vi.mocked(fetch).mock.calls.some(([, init]) => init?.method === 'PUT')).toBe(false);
    expect(nativeConfirm).not.toHaveBeenCalled();
  });

  it('keeps the accounting confirmation open on failure and allows retry from forwarding', async () => {
    const originalFetch = fetch;
    let attempts = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        if (String(input) === `/users/${tenant}/alice/profile` && init?.method === 'PUT') {
          attempts += 1;
          return Promise.resolve(
            attempts === 1
              ? Response.json({ error: '保存失败，请重试' }, { status: 503 })
              : Response.json({ ...users[0], traffic_accounting_mode: 'double' }),
          );
        }
        if (String(input) === '/users?include_disabled=true') {
          return Promise.resolve(
            Response.json({
              users: users.map(user => (user.id === 'alice' ? { ...user, traffic_accounting_mode: 'double' } : user)),
            }),
          );
        }
        if (String(input) === '/model/snapshot') return Promise.resolve(Response.json(snapshot));
        return originalFetch(input, init);
      }),
    );
    const { view } = mount();
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    const control = await view.findByRole('group', { name: '用户流量计量口径' });
    fireEvent.click(within(control).getByRole('button', { name: '双向' }));
    const confirmation = view.getByRole('dialog', { name: '切换为双向计量' });
    fireEvent.click(within(confirmation).getByRole('button', { name: '切换并重算' }));
    expect(await within(confirmation).findByText(/保存失败，请重试/)).toBeTruthy();
    expect(within(control).getByRole('button', { name: '单向', pressed: true })).toBeTruthy();
    fireEvent.click(within(confirmation).getByRole('button', { name: '切换并重算' }));
    await vi.waitFor(() => expect(view.queryByRole('dialog', { name: '切换为双向计量' })).toBeNull());
    expect(attempts).toBe(2);
    expect(within(control).getByRole('button', { name: '双向', pressed: true })).toBeTruthy();
  });

  it('formats the current online epoch without falling back to historical first-seen time', () => {
    const started = '2026-10-19T00:00:00Z';
    expect(sessionDurationText(started, Date.parse('2026-10-19T00:00:30Z'))).toBe('30秒');
    expect(sessionDurationText(started, Date.parse('2026-10-19T00:01:00Z'))).toBe('1分');
    expect(sessionDurationText(started, Date.parse('2026-10-19T03:12:00Z'))).toBe('3小时 12分');
    expect(sessionDurationText(started, Date.parse('2026-10-21T05:00:00Z'))).toBe('2天 5小时');
    expect(sessionDurationText(undefined, Date.now())).toBe('—');
  });

  it('groups cards in the confirmed order and exposes a single panel at a time', async () => {
    const { view } = mount();
    const expected = render(<Icon of="forwarding" />).container.querySelector('svg')!.innerHTML;
    expect(view.getAllByRole('tab').map(tab => tab.textContent)).toEqual(['接入', '转发', '统计']);
    expect(view.getByRole('tab', { name: '转发' }).querySelector('svg')?.innerHTML).toBe(expected);
    const titles = () =>
      [...view.getByRole('tabpanel').querySelectorAll(':scope > .panel > :is(header, summary) h4')].map(
        title => title.textContent,
      );
    // 订阅与节点（含网络拨测）回到标题栏按钮打开的弹窗，不占「接入」页签的位置；来源封禁并入在线设备。
    expect(titles()).toEqual(['用量与额度', '在线设备', '接入授权']);
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    expect(titles()).toEqual(['用量与额度', '转发入口']);
    expect(view.getByRole('heading', { name: '转发入口' }).querySelector('svg')?.innerHTML).toBe(expected);
    fireEvent.click(view.getByRole('tab', { name: '统计' }));
    expect(titles()).toEqual(['实时流量', '机器流量', '站点流量']);
    const selected = view.getByRole('tab', { selected: true });
    expect(view.getByRole('tabpanel').getAttribute('aria-labelledby')).toBe(selected.id);
    expect(selected.getAttribute('aria-controls')).toBe(view.getByRole('tabpanel').id);
  });

  it('opens the subscription and node dialog from the detail header and reads the address only then', async () => {
    const { view } = mount();
    const reads = () => vi.mocked(fetch).mock.calls.map(([input]) => String(input));
    expect(reads().some(isSubscriptionRead)).toBe(false);
    expect(reads()).not.toContain('/grant-probes/capability');

    fireEvent.click(view.getByRole('button', { name: '订阅' }));
    const dialog = await view.findByRole('dialog', { name: 'alice 的订阅与节点' });
    expect(within(dialog).getByRole('tab', { name: '订阅' }).getAttribute('aria-selected')).toBe('true');
    expect(await within(dialog).findByText('Clash 订阅')).toBeTruthy();
    expect(reads()).toContain(`/users/${tenant}/alice/clash-subscription`);

    // 「节点」页签并入网络拨测：页签内读取本机拨测能力。
    fireEvent.click(within(dialog).getByRole('tab', { name: '节点' }));
    expect(within(dialog).getByRole('tab', { name: '节点' }).getAttribute('aria-selected')).toBe('true');
    await vi.waitFor(() => expect(reads()).toContain('/grant-probes/capability'));
  });

  it('opens the node tab directly and keeps the entries disabled for roles that cannot read artifacts', async () => {
    const admin = mount();
    fireEvent.click(admin.view.getByRole('button', { name: '节点' }));
    const dialog = await admin.view.findByRole('dialog', { name: 'alice 的订阅与节点' });
    expect(within(dialog).getByRole('tab', { name: '节点' }).getAttribute('aria-selected')).toBe('true');
    cleanup();

    const reviewer = mount('readonly');
    expect((reviewer.view.getByRole('button', { name: '节点' }) as HTMLButtonElement).disabled).toBe(true);
    expect((reviewer.view.getByRole('button', { name: '订阅' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('keeps availability and a compact UUID in the header while usage stays in the ledger', async () => {
    const uuid = '1ec1d194-83bd-4fee-9da2-2898771b2eaa';
    const { view, selectUser, client } = mount();
    await seedUsers(
      client,
      users.map(user => (user.id === 'alice' ? { ...user, uuid } : user)),
    );
    const header = () => view.container.querySelector<HTMLElement>('.user-dhead-main')!;
    expect(
      within(header())
        .getAllByRole('tab')
        .map(tab => tab.textContent),
    ).toEqual(['接入', '转发', '统计']);
    expect(
      [...header().querySelectorAll('.user-dhead-tools button')].map(
        button => button.getAttribute('aria-label') ?? button.textContent,
      ),
    ).toEqual(['节点', '订阅', '更多操作']);
    const status = within(header()).getByRole('status', { name: /可用/ });
    expect(status.textContent).toBe('可用');
    expect(status.getAttribute('title')).toContain('个接入点');
    const copy = within(header()).getByRole('button', { name: '复制完整 UUID' });
    expect(copy.textContent).toBe('UUID1ec1d194…2eaa');
    expect(within(copy).getByTitle(uuid)).toBeTruthy();
    expect(header().textContent).not.toContain('54.10 GiB');
    expect(view.getByRole('region', { name: '本期接入' }).querySelector('.user-usage-hero-value')?.textContent).toBe(
      '54.10 GiB',
    );
    expect(header().querySelector('.dstat, .user-duuid, .segsw')).toBeNull();
    expect(within(header()).queryByText('修改密码')).toBeNull();

    act(() => selectUser('carol'));
    expect(within(header()).getByText('测试').classList.contains('test')).toBe(true);
    expect(within(header()).getByRole('status').textContent).toBe('未授权');
    act(() => selectUser('erin'));
    expect(within(header()).getByRole('status', { name: /已停用/ }).textContent).toBe('已停用');
  });

  it('copies the full UUID and resets feedback when selecting another user', async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
    const aliceUuid = '00000000-0000-4000-8000-000000000001';
    const carolUuid = '00000000-0000-4000-8000-000000000002';
    const { view, client, selectUser } = mount();
    await seedUsers(
      client,
      users.map(user => ({ ...user, uuid: user.id === 'alice' ? aliceUuid : carolUuid })),
    );
    const header = () => within(view.container.querySelector<HTMLElement>('.user-dhead-main')!);
    await act(async () => fireEvent.click(header().getByRole('button', { name: '复制完整 UUID' })));
    expect(writeText).toHaveBeenLastCalledWith(aliceUuid);
    expect(header().getByRole('button', { name: 'UUID 已复制' })).toBeTruthy();
    act(() => selectUser('carol'));
    const copy = header().getByRole('button', { name: '复制完整 UUID' });
    expect(copy.textContent).toContain('00000000…0002');
    await act(async () => fireEvent.click(copy));
    expect(writeText).toHaveBeenLastCalledWith(carolUuid);
  });

  it('shows a missing UUID without offering a copy action', async () => {
    const { view, client } = mount();
    await seedUsers(
      client,
      users.map(user => ({ ...user, uuid: undefined })),
    );
    const header = within(view.container.querySelector<HTMLElement>('.user-dhead-main')!);
    expect(header.getByText('UUID —')).toBeTruthy();
    expect(header.queryByRole('button', { name: '复制完整 UUID' })).toBeNull();
  });

  it.each([
    ['pending', '未生效'],
    ['expired', '已到期'],
  ] as const)('keeps %s service status distinct from available access', async (serviceState, label) => {
    const { view, client } = mount();
    await seedUsers(
      client,
      users.map(user => (user.id === 'alice' ? { ...user, service_state: serviceState } : user)),
    );
    const header = within(view.container.querySelector<HTMLElement>('.user-dhead-main')!);
    const status = header.getByRole('status', { name: /无法连接/ });
    expect(status.textContent).toBe(label);
    expect(status.getAttribute('data-tone')).toBe('bad');
  });

  it('collects UUID, account type, password and lifecycle actions in the more menu', async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
    const uuid = '1ec1d194-83bd-4fee-9da2-2898771b2eaa';
    const withAlice = (extra: Partial<UserListItem>) =>
      users.map(user => (user.id === 'alice' ? { ...user, uuid, login_enabled: true, ...extra } : user));
    const originalFetch = fetch;
    const profileWrites: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === `/users/${tenant}/alice/profile` && init?.method === 'PUT') {
          profileWrites.push(JSON.parse(String(init.body)));
          return Promise.resolve(Response.json({ ...person('alice'), uuid, account_type: 'test' }));
        }
        if (path === '/users?include_disabled=true')
          return Promise.resolve(Response.json({ users: withAlice({ account_type: 'test' }) }));
        if (path === '/model/snapshot') return Promise.resolve(Response.json(snapshot));
        return originalFetch(input, init);
      }),
    );
    const { view, client } = mount();
    await seedUsers(client, withAlice({}));

    fireEvent.click(view.getByRole('button', { name: '更多操作' }));
    const menu = view.getByRole('menu');
    expect(
      within(menu)
        .getAllByRole('menuitem')
        .map(item => item.firstElementChild?.nextSibling?.textContent),
    ).toEqual([
      `复制 UUID${uuid}`,
      '标为测试账号名册中单独标注，可按「测试」筛选',
      '修改密码为该用户设置新的登录密码',
      '重置登录密码生成一次性密码并注销现有会话',
      '生成直达登录页无需输入密码即可登录',
      '停用用户旧凭据将立即失效',
      '更换 UUID当前连接将断开，订阅需要重新导入',
    ]);

    // 复制 UUID 不关闭菜单，结果直接写在这一项上。
    fireEvent.click(within(menu).getByRole('menuitem', { name: new RegExp(`^复制 UUID${uuid}$`) }));
    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith(uuid));
    expect(await within(menu).findByRole('menuitem', { name: new RegExp(`^UUID 已复制${uuid}$`) })).toBeTruthy();
    expect(view.getByRole('menu')).toBe(menu);

    // 账号类型从标题栏的开关移进菜单：一项切换，写入后菜单关闭。
    fireEvent.click(within(menu).getByRole('menuitem', { name: /^标为测试账号/ }));
    expect(view.queryByRole('menu')).toBeNull();
    await vi.waitFor(() => expect(profileWrites).toEqual([{ account_type: 'test' }]));

    fireEvent.click(view.getByRole('button', { name: '更多操作' }));
    fireEvent.click(view.getByRole('menuitem', { name: /^修改密码/ }));
    expect(await view.findByRole('dialog', { name: /alice/ })).toBeTruthy();
  });

  it('opens the more menu for read-only reviewers only to copy the UUID', async () => {
    const { view, client } = mount('readonly');
    const uuid = '1ec1d194-83bd-4fee-9da2-2898771b2eaa';
    // 没有 UUID 时菜单里没有可用项，按钮禁用。
    expect((view.getByRole('button', { name: '更多操作' }) as HTMLButtonElement).disabled).toBe(true);
    await seedUsers(
      client,
      users.map(user => (user.id === 'alice' ? { ...user, uuid } : user)),
    );
    fireEvent.click(view.getByRole('button', { name: '更多操作' }));
    const items = within(view.getByRole('menu')).getAllByRole('menuitem') as HTMLButtonElement[];
    expect(items.filter(item => !item.disabled).map(item => item.textContent)).toEqual([`复制 UUID${uuid}`]);
    expect(items.some(item => /停用用户|重置登录密码|生成直达登录页/.test(item.textContent ?? ''))).toBe(false);
  });

  it('moves keyboard focus through the more menu and returns it on Escape', async () => {
    const { view } = mount();
    const trigger = view.getByRole('button', { name: '更多操作' });

    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    const menu = view.getByRole('menu', { name: 'alice 更多操作' });
    const enabledItems = within(menu)
      .getAllByRole('menuitem')
      .filter(item => !(item as HTMLButtonElement).disabled);
    await vi.waitFor(() => expect(document.activeElement).toBe(enabledItems[0]));

    fireEvent.keyDown(document.activeElement!, { key: 'End' });
    expect(document.activeElement).toBe(enabledItems.at(-1));
    fireEvent.keyDown(document.activeElement!, { key: 'Home' });
    expect(document.activeElement).toBe(enabledItems[0]);

    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    await vi.waitFor(() => {
      expect(view.queryByRole('menu', { name: 'alice 更多操作' })).toBeNull();
      expect(document.activeElement).toBe(trigger);
    });
  });

  it('supports roving keyboard focus without changing the URL and retains the tab across users', () => {
    const { view, selectUser } = mount();
    const before = location.href;
    const access = view.getByRole('tab', { name: '接入' });
    access.focus();
    fireEvent.keyDown(access, { key: 'ArrowLeft' });
    expect(document.activeElement).toBe(view.getByRole('tab', { name: '统计', selected: true }));
    fireEvent.keyDown(document.activeElement!, { key: 'Home' });
    expect(document.activeElement).toBe(access);
    fireEvent.keyDown(access, { key: 'ArrowRight' });
    expect(document.activeElement).toBe(view.getByRole('tab', { name: '转发', selected: true }));
    expect(access.tabIndex).toBe(-1);
    selectUser('dave');
    expect(view.getByRole('tab', { name: '转发', selected: true }).tabIndex).toBe(0);
    fireEvent.keyDown(view.getByRole('tab', { selected: true }), { key: 'End' });
    expect(document.activeElement).toBe(view.getByRole('tab', { name: '统计', selected: true }));
    expect(location.href).toBe(before);
  });

  it('keeps the forwarding filter while switching tabs but resets it for a different user', async () => {
    const { view, selectUser } = mount();
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    const search = await view.findByRole('searchbox', { name: '搜索规则' });
    fireEvent.change(search, { target: { value: 'unsubmitted-filter' } });
    fireEvent.click(view.getByRole('tab', { name: '接入' }));
    expect(view.queryByRole('searchbox', { name: '搜索规则' })).toBeNull();
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    expect(view.getByRole('searchbox', { name: '搜索规则' })).toBe(search);
    expect((search as HTMLInputElement).value).toBe('unsubmitted-filter');
    await act(async () => selectUser('dave'));
    expect(((await view.findByRole('searchbox', { name: '搜索规则' })) as HTMLInputElement).value).toBe('');
  });

  it('places site reports inside the selected user detail, not the user roster or global settings', async () => {
    const { view, selectUser } = mount();
    expect(view.container.querySelector('.user-split-roster .site-usage')).toBeNull();
    await act(async () => selectUser('alice'));
    fireEvent.click(view.getByRole('tab', { name: '统计' }));
    const report = await view.findByRole('region', { name: '站点流量' });
    // 其他站点与嗅探失败的明细并入站点流量卡，不再是单独的卡片。
    expect(view.queryByRole('region', { name: '其他站点 Top N' })).toBeNull();
    expect(report.closest('.user-split-detail')?.querySelector('.dname')?.textContent).toContain('alice');
    expect(view.queryByRole('region', { name: '全局出流量统计' })).toBeNull();
    expect(
      vi.mocked(fetch).mock.calls.some(([path]) => String(path) === `/users/${tenant}/alice/site-usage?period=day`),
    ).toBe(true);
    await act(async () => selectUser('dave'));
    expect(
      view.getByRole('region', { name: '站点流量' }).closest('.user-split-detail')?.querySelector('.dname')
        ?.textContent,
    ).toContain('dave');
    expect(
      vi.mocked(fetch).mock.calls.some(([path]) => String(path) === `/users/${tenant}/dave/site-usage?period=day`),
    ).toBe(true);
  });

  it('counts forwarding in user totals but does not apply it to line quotas', async () => {
    const { view, row, selectUser, client } = mount();
    await act(async () => {
      client.setQueryData(['user-cycles'], {
        views: [usage('alice', 1)],
        forwarding: [
          {
            tenant_id: tenant,
            user_id: 'alice',
            node_id: 'n1',
            rule_id: 7,
            rule_name: 'Web',
            uplink_bytes: 0,
            downlink_bytes: 2 * GiB,
            has_gap: false,
          },
        ],
      });
      selectUser('alice');
    });
    expect(row('alice').querySelector('.user-row-usage')?.textContent).toContain('3');
    expect(row('alice').querySelector('.user-row-quota')?.textContent).toContain('余裕');
    const access = view.getByRole('region', { name: '本期接入' });
    expect(access.querySelector('.user-usage-hero-value')?.textContent).toBe('1.00 GiB');
    expect(view.getByRole('list', { name: '各线路的用量与额度' }).textContent).not.toContain('Web');
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    expect(view.getByRole('tabpanel', { name: '转发' })).toBeTruthy();
    expect(view.queryByRole('region', { name: '本期接入' })).toBeNull();
  });
  it('lets public visitors read forwarding while keeping statistics and write actions closed', async () => {
    const { view, selectUser } = mount('readonly', 'public');

    await act(async () => {
      selectUser('alice');
      await Promise.resolve();
    });

    expect(view.getAllByRole('tab').map(tab => tab.textContent)).toEqual(['接入', '转发']);
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    const panel = view.getByRole('tabpanel', { name: '转发' });
    expect(within(panel).getByRole('heading', { name: '转发入口' })).toBeTruthy();
    expect(within(panel).queryByRole('button', { name: /新建规则|授权机器|编辑|TCP Ping/ })).toBeNull();
    expect(view.queryByRole('tab', { name: '统计' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => isForwardingRead(String(input)))).toBe(true);
    expect(
      vi
        .mocked(fetch)
        .mock.calls.some(([input]) => /tcp-pings|quota\/jobs|realtime|presence-history/.test(String(input))),
    ).toBe(false);
  });

  it.each([
    [0, '余裕'],
    [49.99, '余裕'],
    [50, '余裕'],
    [74.99, '余裕'],
    [75, '余裕'],
    [79.99, '余裕'],
    [80, '80%'],
    [80.01, '80%'],
    [94.99, '95%'],
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
    const quotaInput = () => view.queryByRole('textbox', { name: '本期流量额度（GiB）' }) as HTMLInputElement | null;
    const roster = view.getByRole('listbox', { name: '用户列表' });
    const aliceDetail = view.container.querySelector('.user-split-detail');
    fireEvent.click(view.getByRole('button', { name: '东京 · 额度与超额处理' }));
    expect(quotaInput()?.value).toBe('100');

    selectUser('dave');
    const daveDetail = view.container.querySelector('.user-split-detail');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(daveDetail).toBe(aliceDetail);
    expect(daveDetail?.querySelector('.dname > b')?.textContent).toBe('dave');
    expect(quotaInput()).toBeNull();
    expect(view.queryByText('加载中…')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '东京 · 额度与超额处理' }));
    expect(quotaInput()?.value).toBe('50');

    selectUser('alice');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(view.container.querySelector('.user-split-detail .dname > b')?.textContent).toBe('alice');
    expect(quotaInput()).toBeNull();
  });

  it('opens quota handling from the app row without an access total editor', () => {
    const { view } = mount();
    const control = view.getByRole('button', { name: '东京 · 额度与超额处理' });
    expect(control.closest('.qta-r')?.textContent).toContain('东京');
    expect(view.queryByRole('button', { name: '设置接入总额度' })).toBeNull();
    fireEvent.click(control);
    const dialog = view.getByRole('form', { name: '东京 · 额度与超额处理' });
    expect(dialog.closest('.qta-r')).toBeTruthy();
    expect(view.queryByRole('dialog')).toBeNull();
    const input = within(dialog).getByRole('textbox', { name: '本期流量额度（GiB）' }) as HTMLInputElement;
    expect(input.value).toBe('100');
    fireEvent.change(input, { target: { value: '120' } });
    expect(input.value).toBe('120');
  });

  it('preserves the inline quota draft while switching detail tabs', () => {
    const { view } = mount();
    fireEvent.click(view.getByRole('button', { name: '东京 · 额度与超额处理' }));
    const input = view.getByRole('textbox', { name: '本期流量额度（GiB）' }) as HTMLInputElement;
    fireEvent.change(input, { target: { value: '120' } });
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    expect(view.queryByRole('textbox', { name: '本期流量额度（GiB）' })).toBeNull();
    fireEvent.click(view.getByRole('tab', { name: '接入' }));
    expect(view.getByRole('textbox', { name: '本期流量额度（GiB）' })).toBe(input);
    expect(input.value).toBe('120');
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
    expect(tokyo.querySelector('.qta-left-value')?.textContent).toBe('剩余45.90 GiB');
    expect(card().querySelector('.user-dcard-foot')).toBeNull();

    // dave 用尽额度：执行器撤销的接入点不在 grants 里，只能从额度记录读出。
    selectUser('dave');
    const daveRoute = card().querySelector<HTMLElement>('.qta-r')!;
    expect(daveRoute.classList.contains('over')).toBe(true);
    expect(daveRoute.querySelector('.qta-sub.stop')?.textContent).toBe('系统已停用 1 个接入点');
    expect(daveRoute.querySelector('.qta-sub.stop')?.getAttribute('title')).toBe(
      '东京 IIJ 已被系统停用；补足额度或账期重置后自动恢复',
    );
    expect(daveRoute.querySelector('.qta-left-value')?.textContent).toBe('超出419.8 MiB');
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
      if (isForwardingRead(path)) return Response.json(forwardingEmpty(path));
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
            offlineSource('3.3.3.3', today - 4 * 24 * HOUR, ['vless']),
            offlineSource('2.2.2.2', today - 6 * 24 * HOUR, ['anytls']),
            offlineSource('4.4.4.4', today - 20 * 24 * HOUR, ['vless']),
            offlineSource('5.5.5.5', today - 25 * 24 * HOUR, ['vless']),
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
    expect(readingOf(card)).toBe('在线 1 · 30 天 8 个地址');
    expect([...card.querySelectorAll('.user-presence-day')].map(day => day.textContent)).toEqual([
      '在线',
      '今天',
      '昨天',
      '7 天内',
    ]);
    const recent = view.getByText('8.8.8.8').closest<HTMLElement>('.user-presence-row')!;
    expect(recent.classList.contains('off')).toBe(true);
    expect(recent.querySelector('.user-presence-time')?.textContent).toBe('00:01');
    expect(within(recent).getByText('日本 · 教育网')).toBeTruthy();
    const accessToggle = within(recent).getByRole('button', { name: '展开 1 条接入链详情' });
    expect(accessToggle.getAttribute('aria-expanded')).toBe('false');
    expect(within(recent).queryByRole('button', { name: '东京 IIJ' })).toBeNull();
    fireEvent.click(accessToggle);
    expect(accessToggle.getAttribute('aria-expanded')).toBe('true');
    expect(within(recent).getByRole('button', { name: '东京 IIJ' })).toBeTruthy();
    expect(within(recent).getByText('Hysteria2')).toBeTruthy();
    expect(within(recent).getAllByText('3 KiB').length).toBeGreaterThan(0);
    expect(within(recent).getByRole('button', { name: '复制历史来源 IP 8.8.8.8' })).toBeTruthy();
    expect(within(card).getByText('9.9.9.9')).toBeTruthy();
    expect(within(card).getByText('2001:db8::5')).toBeTruthy();
    expect(within(card).queryByText('4.4.4.4')).toBeNull();
    expect(within(card).queryByText('5.5.5.5')).toBeNull();

    const older = within(card).getByRole('button', { name: '显示其余 2 条历史记录' });
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
    fireEvent.click(within(card).getByRole('button', { name: '收起历史记录' }));
    expect(within(card).queryByText('4.4.4.4')).toBeNull();
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === historyPath)).toHaveLength(1);
  });

  it('lists completed epochs as ordinary timeline rows even when the IP is online again', async () => {
    const fallback = fetch;
    const path = `/users/${tenant}/alice/presence-history`;
    const now = Date.now();
    const sources = [
      { ...offlineSource('1.1.1.1', now - HOUR, ['vless']), session_id: '22' },
      { ...offlineSource('1.1.1.1', now - 2 * HOUR, ['anytls']), session_id: '21', session_downlink_bytes: 4_096 },
      { ...offlineSource('8.8.8.8', now - 3 * HOUR, ['vless']), session_id: '20' },
      { ...offlineSource('8.8.8.8', now - 4 * HOUR, ['hysteria2']), session_id: '19' },
    ];
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
      String(input) === path
        ? Response.json({ tenant_id: tenant, user_id: 'alice', retention_days: 30, truncated: false, sources })
        : fallback(input, init),
    );
    vi.stubGlobal('fetch', fetchMock);
    const { view, client } = mount();
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    await vi.waitFor(() => expect(within(card).getAllByText('1.1.1.1')).toHaveLength(3));
    expect(readingOf(card)).toBe('在线 1 · 30 天 2 个地址');
    const rows = [...card.querySelectorAll<HTMLElement>('.user-presence-row.off')];
    expect(rows).toHaveLength(4);
    expect(rows.map(row => row.querySelector('code')?.textContent)).toEqual([
      '1.1.1.1',
      '1.1.1.1',
      '8.8.8.8',
      '8.8.8.8',
    ]);
    expect(rows[0].querySelector<HTMLElement>('.user-presence-time')?.title).toContain(
      new Date(now - HOUR).toLocaleString(),
    );
    expect(within(rows[1]).getByText('5 KiB')).toBeTruthy();
    expect(within(rows[1]).getByRole('button', { name: '展开 1 条接入链详情' }).textContent).toContain('AnyTLS');
    fireEvent.click(within(rows[1]).getByRole('button', { name: '展开 1 条接入链详情' }));
    expect(rows[1].querySelector('.user-presence-access')).toBeTruthy();
    expect(rows[0].querySelector('.user-presence-access')).toBeNull();
    expect(card.querySelector('details')).toBeNull();
    expect(within(card).queryByText(/更早.*段会话/)).toBeNull();
    // The same IP set with a new Xray epoch still refreshes history.
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    const source = data.users[0].sources[0];
    act(() =>
      client.setQueryData(key, {
        ...data,
        users: [
          {
            ...data.users[0],
            sources: [
              {
                ...source,
                accesses: source.accesses?.map(access => ({ ...access, online_since_at: new Date(now).toISOString() })),
              },
            ],
          },
        ],
      }),
    );
    await vi.waitFor(() => expect(fetchMock.mock.calls.filter(([input]) => String(input) === path)).toHaveLength(2));
  });

  it('merges nearby history across chains while retaining each epoch and counting preview rows after merging', async () => {
    const fallback = fetch;
    const path = `/users/${tenant}/alice/presence-history`;
    const now = Date.now();
    const recent = { ...offlineSource('1.1.1.1', now - HOUR, ['vless']), session_id: 'new' };
    const olderSource = offlineSource('1.1.1.1', now - HOUR - 59 * 60_000, ['anytls']);
    const older = {
      ...olderSource,
      session_id: 'old',
      node_ids: ['n2'],
      ingress_ids: ['mfm-cmi'],
      accesses: [{ ...olderSource.accesses[0], node_id: 'n2', ingress_id: 'mfm-cmi' }],
    };
    const sources = [
      older,
      recent,
      ...Array.from({ length: 5 }, (_, index) => ({
        ...offlineSource('8.8.8.8', now - (index + 3) * HOUR, ['vless']),
        session_id: `other-${index}`,
      })),
    ];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
        String(input) === path
          ? Response.json({ tenant_id: tenant, user_id: 'alice', retention_days: 30, truncated: false, sources })
          : fallback(input, init),
      ),
    );
    const { view } = mount();
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    const summary = await within(card).findByRole('button', { name: '展开 2 条接入链详情' });
    const merged = summary.closest<HTMLElement>('.user-presence-row')!;
    expect(summary.textContent).toContain('VLESS / AnyTLS · 2 段记录');
    expect(within(merged).getByLabelText('合计流量 6 KiB')).toBeTruthy();
    expect(within(card).getAllByText('1.1.1.1')).toHaveLength(2); // Current online row stays separate.
    expect(card.querySelectorAll('.user-presence-row.off')).toHaveLength(5);
    expect(readingOf(card)).toBe('在线 1 · 30 天 2 个地址');
    fireEvent.click(summary);
    const details = merged.querySelector<HTMLElement>('.user-presence-access')!;
    expect(within(details).getByRole('button', { name: '东京 IIJ' })).toBeTruthy();
    expect(within(details).getByRole('button', { name: '澳门 CMI' })).toBeTruthy();
    expect(details.querySelectorAll('time')).toHaveLength(2);
    expect(within(details).getAllByLabelText('本次会话流量 3 KiB')).toHaveLength(2);
    fireEvent.click(within(card).getByRole('button', { name: '显示其余 1 条历史记录' }));
    expect(card.querySelectorAll('.user-presence-row.off')).toHaveLength(6);
  });

  it('keeps masked epochs separate when their address prefixes look identical', async () => {
    const fallback = fetch;
    const path = `/users/${tenant}/alice/presence-history`;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
        String(input) === path
          ? Response.json({
              tenant_id: tenant,
              user_id: 'alice',
              retention_days: 30,
              truncated: true,
              sources: [
                { ...offlineSource('1.1.1.1', Date.now() - HOUR, ['vless']), session_id: '1' },
                { ...offlineSource('1.1.1.1', Date.now() - HOUR - 60_000, ['anytls']), session_id: '2' },
              ],
            })
          : fallback(input, init),
      ),
    );
    const { view } = mount('readonly', 'operator', true);
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(await within(card).findByText('记录较多，仅显示最近 256 段已结束会话。')).toBeTruthy();
    expect(within(card).getAllByText('1.1.1.1')).toHaveLength(3);
    expect(card.querySelectorAll('details')).toHaveLength(0);
    expect(readingOf(card)).toBe('在线 1');
  });

  it('supports legacy address snapshots and summarizes an offline user by the last appearance', async () => {
    const historyPath = `/users/${tenant}/alice/presence-history`;
    const today = new Date().setHours(0, 0, 0, 0);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/grant-probes/capability') {
        return Response.json({ available: false, reason: '测试中不发起拨测' });
      }
      if (isForwardingRead(path)) return Response.json(forwardingEmpty(path));
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
    expect(view.queryByText('在线设备')).toBeNull();
    expect(view.queryByText('台湾')).toBeNull();
    expect(view.queryByText('1.1.1.1')).toBeNull();
    expect(view.container.querySelector('.user-presence-card')).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/presence'))).toBe(false);
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).endsWith('/source-access'))).toBe(false);
  });

  it('shows local source location and chain names with working chain navigation', async () => {
    const navigate = vi.spyOn(route, 'navigate').mockReturnValue(true);
    const { view } = mount();
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    expect(within(card).getByText('在线设备')).toBeTruthy();
    await vi.waitFor(() => expect(readingOf(card)).toBe('在线 1 · 30 天 1 个地址'));
    expect(within(card).getByText('台湾')).toBeTruthy();
    expect(within(card).getByRole('img', { name: 'TW 地区旗' })).toBeTruthy();
    expect(within(card).getByText('东京 IIJ · 协议未知')).toBeTruthy();
    expect(within(card).queryByText('东京测试节点')).toBeNull();
    expect(within(card).getByText('3 KiB')).toBeTruthy();
    expect(within(card).getByLabelText('本次会话流量 3 KiB')).toBeTruthy();
    expect(within(card).queryByLabelText(/^在线时长 /)).toBeNull();
    fireEvent.click(within(card).getByRole('button', { name: '展开 1 条接入链详情' }));
    const chain = within(card)
      .getByRole('button', { name: '东京 IIJ' })
      .closest<HTMLElement>('.user-presence-access-item')!;
    const duration = within(chain).getByLabelText(/^在线时长 /);
    expect(duration.nextElementSibling).toBe(within(chain).getByLabelText('本次会话流量 3 KiB'));
    expect(card.querySelector(':scope .user-presence-row > .user-presence-session .user-presence-duration')).toBeNull();
    fireEvent.click(within(card).getByRole('button', { name: '东京 IIJ' }));
    expect(navigate).toHaveBeenCalledExactlyOnceWith('chains', { p: 'chain', app: 'tokyo', chain: 'jp-iij' });
    fireEvent.click(within(card).getByRole('button', { name: '收起 1 条接入链详情' }));
    expect(within(card).queryByLabelText(/^在线时长 /)).toBeNull();
    expect(within(card).getByLabelText('本次会话流量 3 KiB')).toBeTruthy();
    expect(view.container.querySelector('.user-account-state')?.getAttribute('aria-label')).toContain('可用');
    expect(view.container.querySelector('.user-dhead .dsub')?.textContent).not.toContain('接入点');
    expect(backgroundReadsOnly()).toBe(true);
  });

  it('hides unreported device traffic even when the session start is known', async () => {
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
                session_uplink_bytes: undefined,
                session_downlink_bytes: undefined,
              },
            ],
          },
        ],
      }),
    );

    await vi.waitFor(() => {
      const row = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-row')!;
      expect(row.querySelector(':scope > .user-presence-session')).toBeNull();
    });
  });

  it('summarizes shared source entry points and expands every chain without recounting the address', async () => {
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
              {
                ...source,
                node_ids: ['n1', 'n2'],
                ingress_ids: ['tyo-iij', 'mfm-cmi'],
                accesses: [
                  { ...source.accesses![0], node_id: 'n1', ingress_id: 'tyo-iij' },
                  { ...source.accesses![0], node_id: 'n2', ingress_id: 'mfm-cmi' },
                ],
                session_uplink_bytes: 2_048,
                session_downlink_bytes: 4_096,
              },
              {
                ...source,
                ip: '2001:db8::1',
                node_ids: ['n2'],
                ingress_ids: ['mfm-cmi'],
                accesses: [{ ...source.accesses![0], node_id: 'n2', ingress_id: 'mfm-cmi' }],
              },
            ],
          },
        ],
      }),
    );
    const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
    await vi.waitFor(() => expect(readingOf(card)).toBe('在线 2 · 30 天 2 个地址'));
    const shared = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-row')!;
    expect(within(shared).getByText('2 条接入链 · 协议未知')).toBeTruthy();
    expect(within(shared).queryByRole('button', { name: '东京 IIJ' })).toBeNull();
    const accessToggle = within(shared).getByRole('button', { name: '展开 2 条接入链详情' });
    fireEvent.click(accessToggle);
    expect(accessToggle.getAttribute('aria-expanded')).toBe('true');
    expect(within(shared).getByRole('button', { name: '东京 IIJ' })).toBeTruthy();
    expect(within(shared).getByRole('button', { name: '澳门 CMI' })).toBeTruthy();
    expect(within(shared).getAllByText('协议未知')).toHaveLength(2);
    expect(within(shared).getByText('位置未知')).toBeTruthy();
    fireEvent.click(within(shared).getByRole('button', { name: '收起 2 条接入链详情' }));
    expect(accessToggle.getAttribute('aria-expanded')).toBe('false');
    expect(within(shared).queryByRole('button', { name: '东京 IIJ' })).toBeNull();
    const v6 = view.getByText('2001:db8::1').closest<HTMLElement>('.user-presence-row')!;
    expect(within(v6).getByText('澳门')).toBeTruthy();
  });

  it('shows observed protocols in expanded details and keeps unknown legacy observations explicit', async () => {
    const { view, client } = mount();
    const key = ['user-presence', 'system-admin'];
    const data = client.getQueryData<UserPresenceList>(key)!;
    const row = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-row')!;
    expect(within(row).getByText('东京 IIJ · 协议未知')).toBeTruthy();
    fireEvent.click(within(row).getByRole('button', { name: '展开 1 条接入链详情' }));
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
                  { node_id: 'n1', ingress_id: 'tyo-iij', protocols: ['vless', 'anytls'] },
                  { node_id: 'n2', ingress_id: 'mfm-cmi', protocols: null },
                ],
              },
            ],
          },
        ],
      }),
    );
    expect(await within(row).findByText('VLESS · AnyTLS')).toBeTruthy();
    expect(within(row).getAllByText('协议未知').length).toBeGreaterThan(0);
    expect(backgroundReadsOnly()).toBe(true);
  });

  it('keeps protocols associated with their own chain', async () => {
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
                  { node_id: 'n1', ingress_id: 'tyo-iij', protocols: ['vless'] },
                  { node_id: 'n2', ingress_id: 'mfm-cmi', protocols: ['hysteria2', 'anytls'] },
                ],
              },
            ],
          },
        ],
      }),
    );
    const row = view.getByText('1.1.1.1').closest<HTMLElement>('.user-presence-row')!;
    fireEvent.click(await within(row).findByRole('button', { name: '展开 2 条接入链详情' }));
    const tokyo = (await within(row).findByRole('button', { name: '东京 IIJ' })).closest<HTMLElement>(
      '.user-presence-access-item',
    )!;
    const macau = within(row)
      .getByRole('button', { name: '澳门 CMI' })
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

  it('marks incomplete online counts as lower bounds without exposing node IDs', async () => {
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
    expect(within(card).getByText(/2 台入口机器中，1 台统计已更新/)).toBeTruthy();
    expect(within(card).getByText(/在线数量可能少算；不代表机器离线/)).toBeTruthy();
    fireEvent.click(within(card).getByRole('button', { name: '展开 1 条接入链详情' }));
    expect(within(card).getByRole('button', { name: '东京 IIJ' })).toBeTruthy();
    expect(within(card).queryByText('retired-node')).toBeNull();
    expect(within(card).getByText('位置未知')).toBeTruthy();
  });

  it.each(['partial', 'unavailable'] as const)(
    'identifies stale and never-reported ingress machines for %s source coverage',
    async state => {
      const { view, client } = mount();
      const key = ['user-presence', 'system-admin'];
      const data = client.getQueryData<UserPresenceList>(key)!;
      act(() =>
        client.setQueryData(key, {
          ...data,
          users: [
            {
              ...data.users[0],
              state,
              expected_nodes: state === 'partial' ? 3 : 2,
              reporting_nodes: state === 'partial' ? 1 : 0,
              missing_nodes: [
                { node_id: 'n1', last_reported_at: new Date(Date.now() - 13 * 60_000).toISOString() },
                { node_id: 'n2', last_reported_at: null },
              ],
            },
          ],
        }),
      );
      const card = view.container.querySelector<HTMLElement>('.user-presence-card')!;
      const note = await within(card).findByText(/在线来源统计不完整/);
      expect(note.textContent).toContain('东京测试节点（上次更新');
      expect(note.textContent).toContain('澳门测试节点（尚未收到统计）');
      expect(note.textContent).toContain('在线数量可能少算；不代表机器离线');
      expect(note.textContent).not.toContain('快照');
    },
  );

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
      if (isForwardingRead(path)) return Promise.resolve(Response.json(forwardingEmpty(path)));
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

  it('hides realtime user rates from self-service users', () => {
    const { view } = mount('user');
    expect(view.queryByRole('tab', { name: '统计' })).toBeNull();
    expect(view.queryByRole('region', { name: '实时流量' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/realtime/'))).toBe(false);
    expect(view.queryByRole('region', { name: '站点流量' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/site-usage'))).toBe(false);
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/machine-usage'))).toBe(false);
    expect(view.queryByRole('region', { name: '其他站点 Top N' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/other-domains'))).toBe(false);
  });

  it.each(['readonly', 'editor', 'publisher', 'tenant-admin', 'system-admin'] as AdminRole[])(
    'shows realtime user rates to signed-in %s operators',
    role => {
      const { view } = mount(role);
      fireEvent.click(view.getByRole('tab', { name: '统计' }));
      expect(view.getByRole('region', { name: '实时流量' })).toBeTruthy();
    },
  );

  it('lays out issued direct-login credentials with separately named copy actions', async () => {
    const originalFetch = fetch;
    const uuid = '11111111-2222-4333-8444-555555555555';
    const token = 'broc_login_fixture_credential_only';
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        if (String(input) === `/users/${tenant}/alice/direct-login`)
          return Promise.resolve(Response.json({ operator_id: 'alice', uuid, token, sessions_revoked: 0 }));
        if (String(input) === '/users?include_disabled=true') return Promise.resolve(Response.json({ users }));
        if (String(input) === '/model/snapshot') return Promise.resolve(Response.json(snapshot));
        return originalFetch(input, init);
      }),
    );
    const { view } = mount();
    fireEvent.click(view.getByRole('button', { name: '更多操作' }));
    fireEvent.click(view.getByRole('menuitem', { name: /生成直达登录页/ }));
    const dialog = await view.findByRole('dialog', { name: 'alice 的直达登录页面' });
    expect(within(dialog).getByRole('button', { name: '复制链接' })).toBeTruthy();
    expect(within(dialog).getByRole('button', { name: '复制 UUID' })).toBeTruthy();
    expect(within(dialog).getByRole('button', { name: '复制 TOKEN' })).toBeTruthy();
    expect(dialog.querySelector('.user-direct-login-field.primary code')?.textContent).toContain(`${uuid}/${token}`);
    expect(dialog.querySelectorAll('.user-direct-login-credentials .user-direct-login-field')).toHaveLength(2);
    fireEvent.click(within(dialog).getByRole('button', { name: '我已保存' }));
    await vi.waitFor(() => expect(view.queryByRole('dialog', { name: 'alice 的直达登录页面' })).toBeNull());
  });

  it('never subscribes to user rates as a visitor', () => {
    const { view } = mount('readonly', 'public');
    expect(view.queryByRole('tab', { name: '统计' })).toBeNull();
    expect(view.queryByRole('region', { name: '实时流量' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/realtime/'))).toBe(false);
    expect(view.queryByRole('region', { name: '站点流量' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/site-usage'))).toBe(false);
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/machine-usage'))).toBe(false);
    expect(view.queryByRole('region', { name: '其他站点 Top N' })).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).includes('/other-domains'))).toBe(false);
  });
});

describe('traffic policy access', () => {
  it('falls back to public access without forbidden requests when a user leaves their forwarding tab', async () => {
    const { view, selectUser, row } = mount('user', 'alice');
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    await view.findByRole('searchbox', { name: '搜索规则' });
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input) === `/users/${tenant}/alice/forwarding`)).toBe(
      true,
    );
    fireEvent.focus(row('dave'));
    await act(async () => selectUser('dave'));
    expect(view.queryByRole('tab', { name: '转发' })).toBeNull();
    expect(view.container.querySelector('.user-forwarding-usage')).toBeNull();
    expect(view.container.querySelector('.dname > b')?.textContent).toBe('dave');
    expect(view.getByRole('tabpanel').getAttribute('aria-label')).toBe('接入');
    expect(
      vi.mocked(fetch).mock.calls.some(([input]) => /\/dave\/(forwarding|traffic-policy)$/.test(String(input))),
    ).toBe(false);
    expect(view.queryByText('当前账号不能查看这项内容')).toBeNull();
    await act(async () => selectUser('alice'));
    expect(view.getByRole('tab', { name: '转发', selected: true })).toBeTruthy();
    await view.findByRole('searchbox', { name: '搜索规则' });
  });

  it('prefetches on focus and replaces complete details without a loading frame', async () => {
    let finish!: (value: Response) => void;
    const response = new Promise<Response>(resolve => {
      finish = resolve;
    });
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
        String(input) === `/users/${tenant}/dave/traffic-policy` ? response : original(input, init),
      ),
    );
    const { view, client, selectUser, row } = mount();
    const roster = view.getByRole('listbox', { name: '用户列表' });
    const detail = view.container.querySelector('.user-split-detail');
    const header = detail!.querySelector('.user-dhead');
    const tabs = detail!.querySelector('.user-detail-tabs');
    const card = detail!.querySelector('.user-usage-card');
    const body = card!.querySelector('.user-usage');
    const previousContent = body!.textContent;
    client.removeQueries({ queryKey: userTrafficPolicyKey(person('dave')), exact: true });
    fireEvent.focus(row('dave'));
    await vi.waitFor(() =>
      expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input).endsWith('/dave/traffic-policy'))).toBe(true),
    );
    expect(detail!.querySelector('.dname')?.textContent).toContain('alice');
    selectUser('dave');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(view.container.querySelector('.user-split-detail')).toBe(detail);
    expect(detail!.querySelector('.user-dhead')).toBe(header);
    expect(detail!.querySelector('.user-detail-tabs')).toBe(tabs);
    expect(detail!.querySelector('.user-usage-card')).toBe(card);
    expect(card!.querySelector('.user-usage')).toBe(body);
    expect(detail!.querySelector('.dname')?.textContent).toContain('alice');
    expect(body!.textContent).toBe(previousContent);
    expect(body!.hasAttribute('aria-busy')).toBe(false);
    expect(detail!.hasAttribute('inert')).toBe(true);
    expect(view.queryByRole('status', { name: '读取用量与额度…' })).toBeNull();
    expect(body!.querySelectorAll('.qta-r')).toHaveLength(1);
    expect(
      vi.mocked(fetch).mock.calls.filter(([input]) => String(input).endsWith('/dave/traffic-policy')),
    ).toHaveLength(1);
    await act(async () =>
      finish(
        Response.json([
          {
            kind: 'access',
            app_id: 'tokyo',
            policy: { quota_bytes: String(50 * GiB), exhausted_action: { kind: 'suspend' } },
            used_bytes: String(50.41 * GiB),
            exhausted: true,
            service_available: true,
            unsupported_nodes: [],
          },
        ]),
      ),
    );
    await view.findByText('已停用', { selector: '.qta-policy-flag' });
    expect(detail!.querySelector('.dname')?.textContent).toContain('dave');
    expect(view.getByRole('listbox', { name: '用户列表' })).toBe(roster);
    expect(view.container.querySelector('.user-split-detail')).toBe(detail);
    expect(card!.querySelector('.user-usage')).toBe(body);
    expect(body!.hasAttribute('aria-busy')).toBe(false);
    expect(detail!.hasAttribute('inert')).toBe(false);
  });

  it('settles a failed selection with its own error instead of retaining the previous user forever', async () => {
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
        String(input).endsWith('/dave/traffic-policy')
          ? Promise.resolve(Response.json({ error: '目标用户策略暂不可用' }, { status: 503 }))
          : original(input, init),
      ),
    );
    const { view, client, selectUser } = mount();
    client.removeQueries({ queryKey: userTrafficPolicyKey(person('dave')), exact: true });
    selectUser('dave');
    const detail = view.container.querySelector('.user-split-detail')!;
    expect(detail.querySelector('.dname')?.textContent).toContain('alice');
    await view.findByText(/目标用户策略暂不可用/);
    expect(detail.querySelector('.dname')?.textContent).toContain('dave');
    expect(detail.hasAttribute('inert')).toBe(false);
    expect(detail.querySelector('.user-usage-loading')).toBeNull();
  });

  it('does not request unopened tabs, including after switching to another user', async () => {
    const { view, selectUser } = mount();
    const reads = () => vi.mocked(fetch).mock.calls.map(([input]) => String(input));
    const unvisitedRead = (path: string) =>
      isForwardingRead(path) || /\/(site-usage|other-domains|sniffing-failed-targets|machine-usage)\?/.test(path);
    await act(async () => {});
    expect(reads().some(unvisitedRead)).toBe(false);
    fireEvent.click(view.getByRole('tab', { name: '转发' }));
    await vi.waitFor(() => expect(reads()).toContain(`/users/${tenant}/alice/forwarding`));
    fireEvent.click(view.getByRole('tab', { name: '统计' }));
    await vi.waitFor(() => expect(reads()).toContain(`/users/${tenant}/alice/site-usage?period=day`));
    fireEvent.click(view.getByRole('tab', { name: '接入' }));
    await act(async () => selectUser('dave'));
    expect(
      reads()
        .filter(path => path.includes('/dave/'))
        .some(unvisitedRead),
    ).toBe(false);
  });

  it('does not replace a newer selection when an earlier policy request finishes late', async () => {
    let finish!: (value: Response) => void;
    const response = new Promise<Response>(resolve => {
      finish = resolve;
    });
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
        String(input).endsWith('/dave/traffic-policy') ? response : original(input, init),
      ),
    );
    const { view, client, selectUser } = mount();
    client.removeQueries({ queryKey: userTrafficPolicyKey(person('dave')), exact: true });
    selectUser('dave');
    expect(view.container.querySelector('.dname')?.textContent).toContain('alice');
    selectUser('carol');
    expect(view.container.querySelector('.dname')?.textContent).toContain('carol');
    await act(async () => finish(Response.json([])));
    expect(view.container.querySelector('.dname')?.textContent).toContain('carol');
  });

  it.each(['policy', 'usage'])('waits for both usage and policies when %s resolves last', async last => {
    let resolvePolicy!: (value: Response) => void;
    let resolveUsage!: (value: Response) => void;
    const policyResponse = new Promise<Response>(resolve => {
      resolvePolicy = resolve;
    });
    const usageResponse = new Promise<Response>(resolve => {
      resolveUsage = resolve;
    });
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        if (String(input).endsWith('/traffic-policy')) return policyResponse;
        if (String(input) === '/usage/user-cycles') return usageResponse;
        return original(input, init);
      }),
    );
    const { view, client } = mount('system-admin', 'operator', false, false, false);
    expect(view.container.querySelector('.qta-r')).toBeNull();
    const policy = () =>
      resolvePolicy(
        Response.json([
          {
            kind: 'access',
            app_id: 'tokyo',
            policy: { quota_bytes: String(100 * GiB), exhausted_action: { kind: 'throttle', rate_bps: 5_000_000 } },
            used_bytes: String(121 * GiB),
            exhausted: true,
            service_available: true,
            unsupported_nodes: [],
          },
        ]),
      );
    const usageReady = () => resolveUsage(Response.json({ views: [usage('alice', 121)] }));
    await act(async () => {
      (last === 'policy' ? usageReady : policy)();
    });
    await vi.waitFor(() =>
      expect(
        client.getQueryState(last === 'policy' ? ['user-cycles'] : userTrafficPolicyKey(person('alice')))?.status,
      ).toBe('success'),
    );
    expect(view.container.querySelector('.qta-r')).toBeNull();
    await act(async () => {
      (last === 'policy' ? policy : usageReady)();
    });
    const state = await view.findByText('限速中 · 5 Mbps');
    expect(state.closest('.qta-r')?.textContent).toContain('超出21.00 GiB');
    expect(state.closest('.qta-r')?.classList.contains('warn')).toBe(true);
    await act(async () => {
      await client.refetchQueries({ queryKey: userTrafficPolicyKey(person('alice')) });
    });
    expect(view.getByText('限速中 · 5 Mbps')).toBe(state);
  });

  it('reads the signed-in user policy while leaving other users on public quota readings', async () => {
    const { view, client, selectUser, row } = mount('user', 'alice', true, false);
    await vi.waitFor(() =>
      expect(vi.mocked(fetch).mock.calls.some(([url]) => String(url) === `/users/${tenant}/alice/traffic-policy`)).toBe(
        true,
      ),
    );
    client.setQueryData(userTrafficPolicyKey(person('erin')), [
      {
        kind: 'access',
        app_id: 'tokyo',
        policy: { quota_bytes: '1024', exhausted_action: { kind: 'throttle', rate_bps: 99_000_000 } },
        used_bytes: '0',
        exhausted: false,
        service_available: true,
        unsupported_nodes: [],
      },
    ]);
    fireEvent.focus(row('erin'));
    selectUser('erin');
    expect(vi.mocked(fetch).mock.calls.some(([url]) => String(url).includes('/erin/traffic-policy'))).toBe(false);
    expect(view.queryByText(/99 Mbps/)).toBeNull();
    selectUser('alice');
    expect(view.queryByText('当前账号不能查看这项内容')).toBeNull();
  });

  it('keeps policy failures inside the quota column alongside the public quota readings', async () => {
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
        String(input).endsWith('/traffic-policy')
          ? Promise.resolve(Response.json({ error: '策略暂不可用' }, { status: 503 }))
          : original(input, init),
      ),
    );
    const { view } = mount('system-admin', 'operator', false, false);
    const accessCard = view.container.querySelector('.user-usage-card:not(.user-forwarding-usage)')!;
    const error = await within(accessCard as HTMLElement).findByText(/策略暂不可用/);
    expect(vi.mocked(fetch).mock.calls.filter(([url]) => String(url).endsWith('/traffic-policy'))).toHaveLength(1);
    const quotas = view.getByRole('list', { name: '各线路的用量与额度' });
    expect(error.closest('.user-access-quotas')).toBe(quotas.parentElement);
    expect(quotas.parentElement?.parentElement?.classList.contains('user-usage')).toBe(true);
    expect(quotas.parentElement?.parentElement?.children).toHaveLength(2);
    expect(quotas.querySelectorAll('.qta-r').length).toBeGreaterThan(0);
  });
});

describe('source bans in 在线设备', () => {
  const banPath = `/users/${tenant}/alice/source-access`;
  const historyPath = `/users/${tenant}/alice/presence-history`;
  const applied = [{ node_id: 'n1', phase: 'applied' as const }];
  // 名单接口的替身：PUT 按版本做乐观并发，成功后相关机器回到「待同步」，与 Console 一致。
  function stubBans(initial: SourceAccessView, history: ReturnType<typeof offlineSource>[] = []) {
    let current = initial;
    const writes: { revision: number; ips: string[] }[] = [];
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        if (path === banPath && init?.method === 'PUT') {
          const body = JSON.parse(String(init.body)) as { revision: number; ips: string[] };
          writes.push(body);
          if (body.revision !== current.revision)
            return Response.json({ error: '封禁名单已被其他操作修改，请刷新后重试' }, { status: 409 });
          current = {
            ...current,
            revision: current.revision + 1,
            ips: [...new Set(body.ips)].sort(),
            nodes: current.nodes.map(node => ({ ...node, phase: 'pending' as const })),
          };
          return Response.json(current);
        }
        if (path === banPath) return Response.json(current);
        if (path === historyPath)
          return Response.json({
            tenant_id: tenant,
            user_id: 'alice',
            retention_days: 30,
            truncated: false,
            sources: history,
          });
        return original(input, init);
      }),
    );
    return {
      writes,
      reads: () =>
        vi.mocked(fetch).mock.calls.filter(([input, init]) => String(input) === banPath && !init?.method).length,
      replace: (next: SourceAccessView) => (current = next),
    };
  }
  const presenceCard = (view: ReturnType<typeof mount>['view']) =>
    view.container.querySelector<HTMLElement>('.user-presence-card')!;
  const rowsWith = (card: HTMLElement, ip: string) =>
    [...card.querySelectorAll<HTMLElement>('.user-presence-row')].filter(row => row.textContent?.includes(ip));

  it('bans an online source from its row after confirming and keeps it marked online until its machine confirms', async () => {
    const bans = stubBans({ revision: 4, ips: [], nodes: applied });
    const { view, client } = mount();
    const card = presenceCard(view);
    fireEvent.click(await within(card).findByRole('button', { name: '封禁 1.1.1.1' }));
    const dialog = view.getByRole('dialog', { name: '封禁 1.1.1.1' });
    expect(within(dialog).getByText('断开该来源的会话，并拒绝再次接入。')).toBeTruthy();
    expect(bans.writes).toHaveLength(0);
    fireEvent.click(within(dialog).getByRole('button', { name: '封禁' }));

    const group = await within(card).findByRole('list', { name: '已封禁的来源' });
    expect(bans.writes).toEqual([{ revision: 4, ips: ['1.1.1.1'] }]);
    // 移出时间线，只在「已封禁」组里出现一次；读数仍按全部来源计，另写封禁数。
    expect(rowsWith(card, '1.1.1.1')).toHaveLength(1);
    expect(rowsWith(card, '1.1.1.1')[0].closest('.source-ban-group')).toBeTruthy();
    expect(readingOf(card)).toContain('在线 1');
    expect(readingOf(card)).toContain('封禁 1');
    expect(within(group).getByText('仍在线 · 东京 IIJ · 协议未知')).toBeTruthy();
    expect(within(group).getByRole('button', { name: '同步中 0/1' })).toBeTruthy();

    // 经过它的机器确认了当前名单：会话已关闭，在线快照还没刷新也不再写「仍在线」。
    bans.replace({ revision: 5, ips: ['1.1.1.1'], nodes: applied });
    await act(async () => {
      await client.refetchQueries({ queryKey: ['source-access', banPath] });
    });
    expect(await within(group).findByRole('button', { name: '已生效' })).toBeTruthy();
    expect(within(group).queryByText(/仍在线/)).toBeNull();
    expect(within(group).getByText('东京 IIJ · 协议未知')).toBeTruthy();
  });

  it('keeps banned IP epochs on the timeline and uses the latest epoch in the ban list', async () => {
    const now = Date.now();
    const recent = { ...offlineSource('9.9.9.9', now - HOUR, ['anytls']), session_id: '22' };
    const earlier = { ...offlineSource('9.9.9.9', now - 2 * HOUR, ['vless']), session_id: '21' };
    stubBans({ revision: 2, ips: ['9.9.9.9'], nodes: applied }, [earlier, recent]);
    const { view } = mount();
    const card = presenceCard(view);
    const group = await within(card).findByRole('list', { name: '已封禁的来源' });
    expect(await within(group).findByText('东京 IIJ · AnyTLS')).toBeTruthy();
    const timeline = within(card).getByRole('list', { name: '在线与最近 30 天的来源' });
    expect(within(timeline).getAllByText('9.9.9.9')).toHaveLength(2);
    expect(within(timeline).queryByRole('button', { name: '封禁 9.9.9.9' })).toBeNull();
    expect(within(group).getAllByRole('button', { name: '解除封禁 9.9.9.9' })).toHaveLength(1);
    expect(card.querySelector('details')).toBeNull();
  });

  it('lists bans with their own region, says when an address has not appeared and unbans in place', async () => {
    const bans = stubBans(
      {
        revision: 2,
        ips: ['198.51.100.23', '9.9.9.9'],
        nodes: applied,
        source_countries: [{ ip: '198.51.100.23', country: 'RU' }],
      },
      [offlineSource('9.9.9.9', Date.now() - 2 * HOUR, ['vless'])],
    );
    const { view } = mount();
    const card = presenceCard(view);
    const group = await within(card).findByRole('list', { name: '已封禁的来源' });
    const never = within(group).getByText('198.51.100.23').closest<HTMLElement>('.user-presence-row')!;
    expect(never.querySelector('.user-presence-location')?.textContent).toBe('俄罗斯');
    expect(await within(never).findByText('30 天内未出现')).toBeTruthy();
    const seen = within(group).getByText('9.9.9.9').closest<HTMLElement>('.user-presence-row')!;
    expect(seen.querySelector('.user-presence-time')?.textContent).toMatch(/^\d{2}[:-]\d{2}$/);
    expect(within(seen).getByText('东京 IIJ · VLESS')).toBeTruthy();
    // 最近出现过的排在前面；历史时间线不再重复列出它。
    expect([...group.querySelectorAll('.user-presence-row code')].map(code => code.textContent)).toEqual([
      '9.9.9.9',
      '198.51.100.23',
    ]);
    expect(rowsWith(card, '9.9.9.9')).toHaveLength(1);

    fireEvent.click(within(group).getByRole('button', { name: '解除封禁 9.9.9.9' }));
    await vi.waitFor(() => expect(bans.writes).toEqual([{ revision: 2, ips: ['198.51.100.23'] }]));
    // 解除后回到历史时间线。
    expect(await within(card).findByRole('button', { name: '复制历史来源 IP 9.9.9.9' })).toBeTruthy();
    expect(rowsWith(card, '9.9.9.9')[0].closest('.source-ban-group')).toBeNull();
  });

  it('adds a typed address with what is known about it, skips duplicates and keeps the input on a conflict', async () => {
    const bans = stubBans({ revision: 1, ips: ['203.0.113.9'], nodes: applied });
    const { view } = mount();
    const card = presenceCard(view);
    fireEvent.click(await within(card).findByRole('button', { name: '封禁其他 IP' }));
    const input = within(card).getByRole<HTMLInputElement>('textbox', { name: '来源 IP' });
    expect(document.activeElement).toBe(input);
    fireEvent.change(input, { target: { value: '1.1.1.1' } });
    expect(within(card).getByText(/台湾 · 在线，封禁后断开/)).toBeTruthy();
    fireEvent.change(input, { target: { value: '::ffff:203.0.113.9' } });
    expect(within(card).getByText('已在名单中')).toBeTruthy();
    expect(within(card).getByRole('button', { name: '封禁' }).hasAttribute('disabled')).toBe(true);

    // 另一位管理员先保存了：这次提交被拒绝，输入保留，名单重新读取后再提交即可。
    bans.replace({ revision: 2, ips: ['198.51.100.1', '203.0.113.9'], nodes: applied });
    fireEvent.change(input, { target: { value: '192.0.2.7' } });
    fireEvent.click(within(card).getByRole('button', { name: '封禁' }));
    expect(await within(card).findByText('封禁名单已被其他操作修改，请刷新后重试')).toBeTruthy();
    expect(input.value).toBe('192.0.2.7');
    expect(await within(card).findByText('198.51.100.1')).toBeTruthy();
    fireEvent.click(within(card).getByRole('button', { name: '封禁' }));
    await vi.waitFor(() =>
      expect(bans.writes.at(-1)).toEqual({ revision: 2, ips: ['198.51.100.1', '203.0.113.9', '192.0.2.7'] }),
    );
    expect(await within(card).findByRole('button', { name: '封禁其他 IP' })).toBeTruthy();
    expect(within(card).getByText('192.0.2.7')).toBeTruthy();
  });

  it('lists the rollout by machine name with the reported failure', async () => {
    stubBans({
      revision: 3,
      ips: ['198.51.100.23'],
      nodes: [
        { node_id: 'n1', phase: 'applied' },
        { node_id: 'n2', phase: 'error', error: '写入策略文件失败' },
      ],
    });
    const { view } = mount();
    const card = presenceCard(view);
    fireEvent.click(await within(card).findByRole('button', { name: '1 台异常' }));
    const nodes = view.getByRole('dialog', { name: '各机器执行情况' });
    expect([...nodes.querySelectorAll('li')].map(item => item.textContent)).toEqual([
      '东京测试节点已生效',
      '澳门测试节点执行异常写入策略文件失败',
    ]);
  });

  it('shows a masked list to a read-only reviewer without matching it against sources or offering changes', async () => {
    stubBans({
      revision: 3,
      ips: ['1.1.1.1', '203.0.***.***', '203.0.***.***'],
      nodes: applied,
      source_countries: [
        { ip: '203.0.***.***', country: 'RU' },
        { ip: '203.0.***.***', country: 'JP' },
      ],
    });
    const { view } = mount('readonly', 'operator', true);
    const card = presenceCard(view);
    const group = await within(card).findByRole('list', { name: '已封禁的来源' });
    // 脱敏后的地址可能同形：不拿它去对照在线来源，时间线保持原样，同形地址也不写地区。
    expect(within(card).getByRole('button', { name: '复制来源 IP 1.1.1.1' }).closest('.source-ban-group')).toBeNull();
    expect(within(group).queryByText(/仍在线/)).toBeNull();
    expect([...group.querySelectorAll('.user-presence-location')].map(location => location.textContent)).toEqual([
      '位置未知',
      '位置未知',
      '位置未知',
    ]);
    expect(within(card).queryByRole('button', { name: /^封禁 / })).toBeNull();
    expect(within(card).queryByRole('button', { name: /^解除封禁/ })).toBeNull();
    expect(within(card).queryByRole('button', { name: /封禁其他 IP|封禁 IP/ })).toBeNull();
  });

  it('reports an unreadable list instead of offering bans against an empty one', async () => {
    const original = fetch;
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
        String(input) === banPath ? Promise.resolve(Response.json({})) : original(input, init),
      ),
    );
    const { view } = mount();
    const card = presenceCard(view);
    expect(await within(card).findByText(/来源封禁响应无效/)).toBeTruthy();
    expect(within(card).queryByRole('button', { name: /^封禁 / })).toBeNull();
    expect(within(card).queryByRole('button', { name: /封禁其他 IP|封禁 IP/ })).toBeNull();
    expect(within(card).queryByRole('list', { name: '已封禁的来源' })).toBeNull();
  });
});
