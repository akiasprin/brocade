import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type {
  ForwardingEntitlement,
  ForwardingOverview,
  ForwardingRule,
  UsageForwardingMonthlyRow,
  UserCycleUsage,
  UserListItem,
} from '../src/api';
import { UserForwardingCard } from '../src/panes/user-forwarding';
import { forwardingQuotaBytes, forwardingQuotaText, forwardingQuotaValue } from '../src/panes/forwarding-quota';
import { Icon } from '../src/ui/icons';

const MiB = 1024 ** 2;
const user = { tenant_id: 'platform.acme', id: 'alice' } as UserListItem;
const BASE = '/users/platform.acme/alice';

const entry = (over: Partial<ForwardingEntitlement> = {}): ForwardingEntitlement => ({
  node_id: 'n1',
  node_name: '东京入口',
  public_ipv4: '198.51.100.10',
  public_ipv6: '2001:4860:4860::8888',
  enabled: true,
  tcp_allowed: true,
  udp_allowed: true,
  listen_port_start: 20_000,
  listen_port_end: 20_100,
  max_rules: 4,
  allow_private_targets: false,
  rule_count: 1,
  desired_generation: 2,
  applied_generation: 2,
  reconcile_status: 'converged',
  last_error: null,
  ...over,
});

const rule = (over: Partial<ForwardingRule> = {}): ForwardingRule => ({
  id: 42,
  tenant_id: 'platform.acme',
  user_id: 'alice',
  node_id: 'n1',
  name: '现有规则',
  protocol: 'tcp',
  listen_port: 20_042,
  // 线上已有规则的 IPv6 目标带方括号存储
  target_host: '[2606:4700:4700::1111]',
  target_port: 443,
  enabled: true,
  runtime_status: 'active',
  uplink_bytes: 1024,
  downlink_bytes: 2048,
  created_at: '2026-10-13 00:00:00.123456+00',
  updated_at: '2026-10-13 00:00:00.123456+00',
  ...over,
});

const overview = (entitlements: ForwardingEntitlement[], rules: ForwardingRule[]): ForwardingOverview => ({
  tenant_id: 'platform.acme',
  user_id: 'alice',
  entitlements,
  rules,
});

const usage = (ruleId: number, down: number, up: number, over: Partial<UsageForwardingMonthlyRow> = {}) => ({
  tenant_id: 'platform.acme',
  user_id: 'alice',
  node_id: 'n1',
  rule_id: ruleId,
  rule_name: `rule-${ruleId}`,
  uplink_bytes: up,
  downlink_bytes: down,
  has_gap: false,
  ...over,
});

const cycleUsage = (rows: UsageForwardingMonthlyRow[]): UserCycleUsage => ({
  periods: [
    { tenant_id: user.tenant_id, user_id: user.id, starts_at: '2026-09-30T16:00:00Z', ends_at: '2026-10-31T16:00:00Z' },
  ],
  views: [],
  forwarding: rows,
});

type Route = (init?: RequestInit) => unknown;

/** 按「方法 路径」应答；未登记的请求返回 404，让查询进入错误态而不是挂起。 */
function serve(routes: Record<string, Route>) {
  const table: Record<string, Route> = {
    'GET /usage/user-cycles': () => cycleUsage([]),
    [`GET ${BASE}/forwarding/tcp-pings`]: () => [],
    'GET /nodes/agent-state': () => ({
      nodes: [
        { node_id: 'n1', name: '东京入口', public_ipv4_country: 'JP' },
        { node_id: 'n2', name: '洛杉矶入口', public_ipv4_country: 'US' },
      ],
    }),
    ...routes,
  };
  const request = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const route = table[`${init?.method ?? 'GET'} ${String(input)}`];
    if (!route) return new Response('Not Found', { status: 404, statusText: 'Not Found' });
    const value = await route(init);
    return value instanceof Response ? value : Response.json(value);
  });
  vi.stubGlobal('fetch', request);
  return request;
}

const writes = (request: ReturnType<typeof serve>) =>
  request.mock.calls
    .filter(([, init]) => init?.method && init.method !== 'GET')
    .map(([input, init]) => ({
      path: String(input),
      method: init?.method,
      body: init?.body ? (JSON.parse(String(init.body)) as unknown) : undefined,
    }));

function show(options: { canAuthorize?: boolean; canManageRules?: boolean; canViewRealtime?: boolean } = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } } });
  const view = render(
    <QueryClientProvider client={client}>
      <UserForwardingCard
        user={user}
        nodes={[
          { id: 'n1', name: '东京入口', api_port: 10085, public_ipv4: '198.51.100.10' },
          { id: 'n2', name: '洛杉矶入口', api_port: 10085 },
        ]}
        canAuthorize={options.canAuthorize ?? false}
        canManageRules={options.canManageRules ?? true}
        canViewRealtime={options.canViewRealtime ?? false}
        accountingDisabled
        onAccountingModeChange={() => undefined}
      />
    </QueryClientProvider>,
  );
  return { ...view, client };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('A inline forwarding regression', () => {
  it('does not report an empty entry as ready after an unsuccessful removal', async () => {
    serve({ [`GET ${BASE}/forwarding`]: () => overview([entry({ reconcile_status: 'error' })], []) });
    const view = show();
    expect(await view.findByText('入口状态待确认')).toBeTruthy();
    expect(view.queryByText('已就绪')).toBeNull();
  });

  it('keeps old rules active while another rule syncs or fails and shows scoped errors', async () => {
    let current = overview(
      [entry({ desired_generation: 3, applied_generation: 2, reconcile_status: 'pending' })],
      [rule(), rule({ id: 43, name: '新增规则', listen_port: 20043, runtime_status: 'pending' })],
    );
    serve({ [`GET ${BASE}/forwarding`]: () => current });
    const view = show();
    await view.findByText('新增规则');
    const old = view.container.querySelector('.fwd-item.st-active')!;
    expect(old.textContent).toContain('已监听');
    expect(old.textContent).not.toContain('同步中');
    expect(view.container.textContent).toContain('1 条正常 · 1 条同步中');
    current = overview(
      [entry({ reconcile_status: 'error', last_error: '另一用户的私密错误' })],
      [
        rule(),
        rule({
          id: 43,
          name: '新增规则',
          listen_port: 20043,
          runtime_status: 'error',
          runtime_error: 'TCP 20043 端口被占用',
        }),
      ],
    );
    await act(() => view.client.invalidateQueries({ queryKey: ['forwarding', user.tenant_id, user.id] }));
    await waitFor(() => expect(view.container.textContent).toContain('1 条正常 · 1 条失败'));
    fireEvent.click(view.getByRole('button', { name: /新增规则，入口 20043/ }));
    expect(view.container.textContent).toContain('TCP 20043 端口被占用');
    expect(view.container.textContent).not.toContain('另一用户的私密错误');
    expect(view.container.querySelector('.fwd-item.st-active')?.textContent).not.toContain('收敛失败');
  });

  it('shows stale rules as unconfirmed with the last report time', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview([entry({ reported_at: '2026-10-21 01:02:03+00' })], [rule({ runtime_status: 'unknown' })]),
    });
    const view = show();
    await view.findByText('现有规则');
    expect(view.container.querySelector('.fwd-item')?.textContent).toContain('状态待确认');
    fireEvent.click(view.getByRole('button', { name: /现有规则，入口 20042/ }));
    expect(view.container.textContent).toContain('上次回报');
    expect(view.container.textContent).not.toContain('已应用这条规则');
  });

  it('warns only when editing an enabled listener rather than its name', async () => {
    serve({ [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]) });
    const view = show();
    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 :20042' }));
    const form = view.getByRole('form', { name: '编辑入口 20042 的规则' });
    fireEvent.change(within(form).getByLabelText('名称'), { target: { value: '改名' } });
    expect(form.textContent).not.toContain('已有连接会断开');
    fireEvent.change(within(form).getByLabelText('目标地址'), { target: { value: 'example.net:8443' } });
    expect(form.textContent).toContain('已有连接会断开；其他规则继续运行');
  });

  it('uses the shared forwarding icon in the panel title and both empty states', async () => {
    serve({ [`GET ${BASE}/forwarding`]: () => overview([], []) });
    const view = show();
    await view.findByText('尚未授权转发入口');
    const expected = render(<Icon of="forwarding" />).container.querySelector('svg')!.innerHTML;
    const icons = view.container.querySelectorAll(
      '.user-forwarding-card .panel-title-icon svg, .empty-state-mark svg, .fwr-empty > .ui-icon svg',
    );
    expect(icons).toHaveLength(3);
    for (const icon of icons) expect(icon.innerHTML).toBe(expected);
  });

  const editAuthorization = async (view: ReturnType<typeof show>, name = '东京入口') => {
    fireEvent.click(await view.findByRole('button', { name: `编辑 ${name} 的转发授权` }));
    return view.getByRole('form', { name: `编辑 ${name} 的转发授权` });
  };

  it('background refresh preserves authorization and rule drafts', async () => {
    let current = overview([entry()], [rule()]);
    serve({ [`GET ${BASE}/forwarding`]: () => current });
    const view = show({ canAuthorize: true });
    const form = await editAuthorization(view);
    fireEvent.change(within(form).getByLabelText('本期转发额度（GiB）'), { target: { value: '150' } });
    current = overview([entry({ cycle_used_bytes: 8 * 1024 ** 3 })], [rule()]);
    await act(() => view.client.invalidateQueries({ queryKey: ['forwarding', user.tenant_id, user.id] }));
    await waitFor(() => expect(view.container.querySelector('.qta-figs')?.textContent).toContain('8.00 GiB'));
    expect(view.getByRole('form')).toBe(form);
    expect((within(form).getByLabelText('本期转发额度（GiB）') as HTMLInputElement).value).toBe('150');
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 :20042' }));
    const ruleForm = view.getByRole('form', { name: '编辑入口 20042 的规则' });
    fireEvent.change(within(ruleForm).getByLabelText('名称'), { target: { value: '保留名称' } });
    current = overview(current.entitlements, [rule({ name: '外部更新的名称' })]);
    await act(() => view.client.invalidateQueries({ queryKey: ['forwarding', user.tenant_id, user.id] }));
    await view.findByText('外部更新的名称');
    expect(view.getByRole('form')).toBe(ruleForm);
    expect((within(ruleForm).getByLabelText('名称') as HTMLInputElement).value).toBe('保留名称');
  });

  it('an externally removed selected machine falls back to all machines', async () => {
    let current = overview(
      [entry(), entry({ node_id: 'n2', node_name: '洛杉矶入口' })],
      [rule(), rule({ id: 43, node_id: 'n2', name: '洛杉矶规则' })],
    );
    serve({ [`GET ${BASE}/forwarding`]: () => current });
    const view = show();
    await view.findByText('洛杉矶规则');
    const filter = view.getByRole('combobox', { name: '筛选转发机器' });
    fireEvent.change(filter, { target: { value: 'n2' } });
    expect(view.container.querySelectorAll('.fwr-item')).toHaveLength(1);
    current = overview([entry()], [rule()]);
    await act(() => view.client.invalidateQueries({ queryKey: ['forwarding', user.tenant_id, user.id] }));
    await view.findByText('现有规则');
    expect((filter as HTMLSelectElement).value).toBe('');
  });

  it('retains a successful policy, preserves the quota draft, and retries only the failed quota', async () => {
    let current = overview([entry({ cycle_limit_bytes: 100 * 1024 ** 3 })], [rule()]);
    let attempts = 0;
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => current,
      [`PUT ${BASE}/forwarding-nodes/n1`]: init => {
        current = overview([{ ...current.entitlements[0], ...JSON.parse(String(init?.body)) }], current.rules);
        return current;
      },
      [`PUT ${BASE}/forwarding-nodes/n1/quota`]: init => {
        if (++attempts === 1) return Response.json({ error: '请重试' }, { status: 503 });
        current = overview(
          [{ ...current.entitlements[0], cycle_limit_bytes: JSON.parse(String(init?.body)).limit_bytes }],
          current.rules,
        );
        return current;
      },
    });
    const view = show({ canAuthorize: true });
    const form = await editAuthorization(view);
    fireEvent.change(within(form).getByLabelText('规则上限'), { target: { value: '8' } });
    fireEvent.change(within(form).getByLabelText('本期转发额度（GiB）'), { target: { value: '200' } });
    fireEvent.click(within(form).getByRole('button', { name: '保存' }));
    await within(form).findByText(/授权已保存，配额保存失败/);
    expect(view.container.querySelector('.qta-left')?.textContent).toContain('1 / 8');
    expect((within(form).getByLabelText('本期转发额度（GiB）') as HTMLInputElement).value).toBe('200');
    expect(writes(request).map(write => write.path)).toEqual([
      `${BASE}/forwarding-nodes/n1`,
      `${BASE}/forwarding-nodes/n1/quota`,
    ]);
    fireEvent.click(within(form).getByRole('button', { name: '保存' }));
    await waitFor(() => expect(view.queryByRole('form')).toBeNull());
    expect(writes(request).map(write => write.path)).toEqual([
      `${BASE}/forwarding-nodes/n1`,
      `${BASE}/forwarding-nodes/n1/quota`,
      `${BASE}/forwarding-nodes/n1/quota`,
    ]);
    expect(writes(request)[2].body).toEqual({ limit_bytes: 200 * 1024 ** 3 });
  });

  it('does not send a quota request after a policy failure', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`PUT ${BASE}/forwarding-nodes/n1`]: () => Response.json({ error: '授权保存失败' }, { status: 503 }),
    });
    const view = show({ canAuthorize: true });
    const form = await editAuthorization(view);
    fireEvent.change(within(form).getByLabelText('规则上限'), { target: { value: '8' } });
    fireEvent.change(within(form).getByLabelText('本期转发额度（GiB）'), { target: { value: '200' } });
    fireEvent.click(within(form).getByRole('button', { name: '保存' }));
    await within(form).findByText('授权保存失败');
    expect(writes(request)).toHaveLength(1);
    expect(view.container.querySelector('.qta-left')?.textContent).toContain('1 / 4');
    expect((within(form).getByLabelText('规则上限') as HTMLInputElement).value).toBe('8');
  });

  it('keeps a newly granted machine editable if its first quota request fails', async () => {
    let current = overview([], []);
    let attempts = 0;
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => current,
      [`PUT ${BASE}/forwarding-nodes/n2`]: init => {
        current = overview(
          [
            entry({
              node_id: 'n2',
              node_name: '洛杉矶入口',
              ...JSON.parse(String(init?.body)),
              cycle_limit_bytes: null,
            }),
          ],
          [],
        );
        return current;
      },
      [`PUT ${BASE}/forwarding-nodes/n2/quota`]: () => {
        if (++attempts === 1) return Response.json({ error: '配额暂不可用' }, { status: 503 });
        current = overview([{ ...current.entitlements[0], cycle_limit_bytes: 50 * 1024 ** 3 }], []);
        return current;
      },
    });
    const view = show({ canAuthorize: true });
    fireEvent.click(await view.findByRole('button', { name: '授权机器' }));
    const grant = view.getByRole('form', { name: '授权转发入口' });
    fireEvent.click(within(grant).getByRole('radio', { name: /洛杉矶入口/ }));
    fireEvent.change(within(grant).getByLabelText('本期转发额度（GiB）'), { target: { value: '50' } });
    fireEvent.click(within(grant).getByRole('button', { name: '授权' }));
    const form = await view.findByRole('form', { name: '编辑 洛杉矶入口 的转发授权' });
    await within(form).findByText(/授权已保存，配额保存失败/);
    expect((within(form).getByLabelText('本期转发额度（GiB）') as HTMLInputElement).value).toBe('50');
    fireEvent.click(within(form).getByRole('button', { name: '保存' }));
    await waitFor(() => expect(view.queryByRole('form')).toBeNull());
    expect(writes(request).map(write => write.path)).toEqual([
      `${BASE}/forwarding-nodes/n2`,
      `${BASE}/forwarding-nodes/n2/quota`,
      `${BASE}/forwarding-nodes/n2/quota`,
    ]);
  });

  it('a previous save finishing cannot close a different editor', async () => {
    let finish: ((value: ForwardingOverview) => void) | undefined;
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`PUT ${BASE}/forwarding-nodes/n1`]: () =>
        new Promise<ForwardingOverview>(resolve => {
          finish = resolve;
        }),
    });
    const view = show({ canAuthorize: true });
    const form = await editAuthorization(view);
    fireEvent.change(within(form).getByLabelText('规则上限'), { target: { value: '8' } });
    fireEvent.click(within(form).getByRole('button', { name: '保存' }));
    await waitFor(() => expect(writes(request)).toHaveLength(1));
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 :20042' }));
    const ruleForm = view.getByRole('form', { name: '编辑入口 20042 的规则' });
    fireEvent.change(within(ruleForm).getByLabelText('名称'), { target: { value: '保留输入' } });
    expect(finish).toBeTypeOf('function');
    finish!(overview([entry({ max_rules: 8 })], [rule()]));
    await waitFor(() => expect(view.container.querySelector('.qta-left')?.textContent).toContain('1 / 8'));
    expect(view.getByRole('form', { name: '编辑入口 20042 的规则' })).toBe(ruleForm);
    expect((within(ruleForm).getByLabelText('名称') as HTMLInputElement).value).toBe('保留输入');
  });

  it('machine filters stay independent from usage and authorization editing', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview(
          [entry(), entry({ node_id: 'n2', node_name: '洛杉矶入口' })],
          [rule(), rule({ id: 43, node_id: 'n2', name: '另一台规则' })],
        ),
      'GET /usage/user-cycles': () => cycleUsage([usage(42, 4 * MiB, 0), usage(43, 9 * MiB, 0, { node_id: 'n2' })]),
    });
    const view = show({ canAuthorize: true });
    await view.findByText('另一台规则');
    const usagePanel = view.container.querySelector('.user-forwarding-usage')!;
    const before = usagePanel.textContent;
    const filter = view.getByRole('combobox', { name: '筛选转发机器' });
    fireEvent.change(filter, { target: { value: 'n2' } });
    expect(usagePanel.textContent).toBe(before);
    const form = await editAuthorization(view);
    expect((filter as HTMLSelectElement).value).toBe('n2');
    fireEvent.change(filter, { target: { value: 'n1' } });
    expect(view.getByRole('form', { name: '编辑 东京入口 的转发授权' })).toBe(form);
    expect(view.queryByText('另一台规则', { exact: true })).toBeNull();
    expect(usagePanel.querySelector('.fwd-quota-pick')).toBeNull();
  });

  it('stopped and full selected machines cannot create on another machine', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview([entry(), entry({ node_id: 'n2', node_name: '洛杉矶入口', enabled: false })], [rule()]),
    });
    const view = show();
    await view.findByText('现有规则');
    fireEvent.change(view.getByRole('combobox', { name: '筛选转发机器' }), { target: { value: 'n2' } });
    const create = view.getByRole('button', { name: '新建规则' }) as HTMLButtonElement;
    expect(create.disabled).toBe(true);
    expect(view.getByText('这台机器已停止转发', { exact: true })).toBeTruthy();
    fireEvent.change(view.getByRole('combobox', { name: '筛选转发机器' }), { target: { value: 'n1' } });
    expect(create.disabled).toBe(false);
    fireEvent.click(create);
    expect(within(view.getByRole('form')).queryByRole('combobox')).toBeNull();
  });

  it('loads quota jobs only on disclosure and preserves pagination', async () => {
    const job = (id: number) => ({
      id,
      node_id: 'n1',
      generation: 2,
      suspended: false,
      status: 'succeeded',
      last_error: null,
      created_at: '2026-10-19 00:00:00+00',
      updated_at: '2026-10-19 00:00:00+00',
    });
    const request = serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview([entry({ quota_job_status: 'succeeded', quota_job_id: 91 })], [rule()]),
      [`GET ${BASE}/forwarding-nodes/n1/quota/jobs`]: () => ({ jobs: [job(91)], next_before_id: 91 }),
      [`GET ${BASE}/forwarding-nodes/n1/quota/jobs?before_id=91`]: () => ({ jobs: [job(77)], next_before_id: null }),
    });
    const view = show({ canAuthorize: true });
    const form = await editAuthorization(view);
    expect(request.mock.calls.some(([input]) => String(input).includes('/quota/jobs'))).toBe(false);
    fireEvent.click(within(form).getByRole('button', { name: '配额工单 · 已完成' }));
    await within(form).findByRole('article', { name: '配额工单 #91' });
    fireEvent.click(within(form).getByRole('button', { name: '更早工单' }));
    await within(form).findByRole('article', { name: '配额工单 #77' });
    fireEvent.click(within(form).getByRole('button', { name: '最新工单' }));
    await within(form).findByRole('article', { name: '配额工单 #91' });
  });

  it('keeps the production TCP Ping action inside the redesigned details', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`POST ${BASE}/forwarding/42/tcp-ping`]: () => ({
        id: 'ping',
        rule_id: 42,
        rule_updated_at: rule().updated_at,
        generation: 2,
        state: 'complete',
        created_at: 1792396800,
        result: { status: 'reachable', code: 'connected', connect_ms: 12, dns_ms: null, resolved_ip: '2001:4860::1' },
      }),
    });
    const view = show();
    await view.findByText('现有规则');
    expect(writes(request)).toHaveLength(0);
    fireEvent.click(view.getByRole('button', { name: /现有规则，入口 20042/ }));
    const probe = view.getByRole('region', { name: '入口 20042 TCP Ping' });
    fireEvent.click(within(probe).getByRole('button', { name: '测试延迟' }));
    await within(probe).findByText(/TCP 连通/);
    expect(writes(request)).toEqual([{ path: `${BASE}/forwarding/42/tcp-ping`, method: 'POST', body: {} }]);
  });
});

describe('forwarding quota input', () => {
  it('converts decimal GiB exactly to safe byte counts and rejects invalid values', () => {
    expect(forwardingQuotaBytes('')).toBe(null);
    expect(forwardingQuotaBytes('  ')).toBe(null);
    expect(forwardingQuotaBytes('100')).toBe(100 * 1024 ** 3);
    expect(forwardingQuotaBytes('1.23')).toBe(Math.round(1.23 * 1024 ** 3));
    for (const value of ['0', '-1', '1.234', 'NaN', '1e3', '9999999999999999999999999', '9'.repeat(100)]) {
      expect(forwardingQuotaBytes(value)).toBe(undefined);
    }
  });

  it('preserves the exact quota bytes when changing only the authorization policy', async () => {
    const exactBytes = 1234567890;
    expect(forwardingQuotaValue(forwardingQuotaText(exactBytes), forwardingQuotaText(exactBytes), exactBytes)).toBe(
      exactBytes,
    );
    const quotaEntry = entry({ cycle_limit_bytes: exactBytes, cycle_used_bytes: 0 });
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([quotaEntry], [rule()]),
      [`PUT ${BASE}/forwarding-nodes/n1`]: () => overview([{ ...quotaEntry, max_rules: 8 }], [rule()]),
    });
    const view = show({ canAuthorize: true });
    fireEvent.click(await view.findByRole('button', { name: '编辑 东京入口 的转发授权' }));
    expect((view.getByLabelText('本期转发额度（GiB）') as HTMLInputElement).value).toBe('1.15');
    fireEvent.change(view.getByLabelText('规则上限'), { target: { value: '8' } });
    fireEvent.click(within(view.getByRole('form')).getByRole('button', { name: '保存' }));
    await waitFor(() => expect(view.queryByRole('form')).toBeNull());
    expect(writes(request)).toHaveLength(1);
    expect(writes(request)[0].path).toBe(`${BASE}/forwarding-nodes/n1`);
  });
});

describe('用户转发入口', () => {
  it('switches between inline editors without disabling unrelated actions and retains the readings', async () => {
    serve({ [`GET ${BASE}/forwarding`]: () => overview([entry({ cycle_used_bytes: 5 * 1024 ** 3 })], [rule()]) });
    const view = show({ canAuthorize: true });
    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 :20042' }));
    expect(
      view
        .getByRole('form', { name: /编辑入口/ })
        .closest('.fwr-item')
        ?.querySelector('.fwd-port')?.textContent,
    ).toContain('20042');
    const quota = view.getByRole('button', { name: '编辑 东京入口 的转发授权' }) as HTMLButtonElement;
    expect(quota.disabled).toBe(false);
    expect((view.getByRole('button', { name: '新建规则' }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(quota);
    expect(view.queryByRole('form', { name: /编辑入口/ })).toBeNull();
    expect(view.getByRole('form', { name: '编辑 东京入口 的转发授权' })).toBeTruthy();
    expect(view.container.querySelector('.qta-figs')?.textContent).toContain('5.00 GiB');
    expect(view.queryAllByRole('form')).toHaveLength(1);
    expect(view.getByRole('button', { name: '编辑 东京入口 :20042' })).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '新建规则' }));
    expect(view.queryAllByRole('form')).toHaveLength(1);
    expect(view.getByRole('form', { name: '新建转发规则' })).toBeTruthy();
  });

  it('quota edits remain inline on failure and Escape cancels without another write', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry({ cycle_limit_bytes: 100 * 1024 ** 3 })], [rule()]),
      [`GET ${BASE}/forwarding-nodes/n1/quota/jobs`]: () => ({ jobs: [], next_before_id: null }),
      [`PUT ${BASE}/forwarding-nodes/n1/quota`]: () => new Response('暂时无法保存', { status: 503 }),
    });
    const view = show({ canAuthorize: true });
    fireEvent.click(await view.findByRole('button', { name: '编辑 东京入口 的转发授权' }));
    const form = view.getByRole('form', { name: '编辑 东京入口 的转发授权' });
    const input = within(form).getByLabelText('本期转发额度（GiB）') as HTMLInputElement;
    expect(document.activeElement).toBe(input);
    fireEvent.change(input, { target: { value: '0' } });
    expect((within(form).getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(input, { target: { value: '25.50' } });
    fireEvent.submit(form);
    expect(await within(form).findByText(/503/)).toBeTruthy();
    expect(input.value).toBe('25.50');
    expect(view.queryByRole('dialog')).toBeNull();
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(view.queryByRole('form', { name: '编辑 东京入口 的转发授权' })).toBeNull();
    expect(writes(request)).toHaveLength(1);
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 的转发授权' }));
    expect((view.getByLabelText('本期转发额度（GiB）') as HTMLInputElement).value).toBe('100.00');
  });

  it('canceling inline stop confirmation leaves both machines and all rules unchanged', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview(
          [entry(), entry({ node_id: 'n2', node_name: '洛杉矶入口' })],
          [rule(), rule({ id: 43, node_id: 'n2' })],
        ),
    });
    const view = show({ canAuthorize: true });
    fireEvent.click(await view.findByRole('button', { name: '编辑 东京入口 的转发授权' }));
    const form = view.getByRole('form');
    fireEvent.click(within(form).getByRole('button', { name: '停止转发' }));
    expect(within(form).getByRole('alert').textContent).toContain('其他用户不受影响');
    expect(view.queryByRole('dialog')).toBeNull();
    fireEvent.click(within(form).getByRole('button', { name: '返回' }));
    expect(writes(request)).toHaveLength(0);
    expect(within(form).getByRole('button', { name: '停止转发' })).toBeTruthy();
    expect((view.getByRole('button', { name: '编辑 洛杉矶入口 的转发授权' }) as HTMLButtonElement).disabled).toBe(
      false,
    );
    expect(view.container.querySelectorAll('.fwr-item')).toHaveLength(2);
  });

  it('lets administrators set a monthly quota per machine and inspect durable jobs', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry({ cycle_used_bytes: 5 * 1024 ** 3 })], [rule()]),
      [`GET ${BASE}/forwarding-nodes/n1/quota/jobs`]: () => ({
        jobs: [
          {
            id: 91,
            node_id: 'n1',
            generation: 3,
            suspended: true,
            status: 'running',
            last_error: '等待节点恢复',
            created_at: '2026-10-19 00:00:00+00',
            updated_at: '2026-10-19 00:00:00+00',
          },
        ],
        next_before_id: null,
      }),
      [`PUT ${BASE}/forwarding-nodes/n1/quota`]: () =>
        overview([entry({ cycle_limit_bytes: 100 * 1024 ** 3, cycle_used_bytes: 5 * 1024 ** 3 })], [rule()]),
    });
    const view = show({ canAuthorize: true });
    fireEvent.click(await view.findByRole('button', { name: '编辑 东京入口 的转发授权' }));
    const dialog = await view.findByRole('form', { name: '编辑 东京入口 的转发授权' });
    expect(view.queryByRole('dialog')).toBeNull();
    expect(dialog.closest('.qta-r.fwe-open')).toBeTruthy();
    fireEvent.click(within(dialog).getByRole('button', { name: /配额工单/ }));
    expect(await within(dialog).findByText(/#91/)).toBeTruthy();
    expect(within(dialog).getByText('等待节点恢复')).toBeTruthy();
    const jobCard = within(dialog).getByRole('article', { name: '配额工单 #91' });
    expect(jobCard.className).toBe('fwd-quota-job');
    expect(within(jobCard).getByText('暂停转发')).toBeTruthy();
    expect(within(jobCard).getByText('等待节点确认')).toBeTruthy();
    expect(within(jobCard).getByText('等待节点恢复')).toBeTruthy();
    fireEvent.change(within(dialog).getByLabelText('本期转发额度（GiB）'), { target: { value: '100' } });
    fireEvent.click(within(dialog).getByRole('button', { name: '保存' }));
    await waitFor(() =>
      expect(writes(request)).toContainEqual({
        path: `${BASE}/forwarding-nodes/n1/quota`,
        method: 'PUT',
        body: { limit_bytes: 100 * 1024 ** 3 },
      }),
    );
    await waitFor(() => expect(view.queryByRole('form', { name: '编辑 东京入口 的转发授权' })).toBeNull());
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 的转发授权' }));
    const reopened = view.getByRole('form', { name: '编辑 东京入口 的转发授权' });
    fireEvent.change(within(reopened).getByLabelText('本期转发额度（GiB）'), { target: { value: '' } });
    fireEvent.click(within(reopened).getByRole('button', { name: '保存' }));
    await waitFor(() =>
      expect(writes(request)).toContainEqual({
        path: `${BASE}/forwarding-nodes/n1/quota`,
        method: 'PUT',
        body: { limit_bytes: null },
      }),
    );
  });

  it('shows quota suspension distinctly from a manual disable and hides administrator controls', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview(
          [entry({ quota_suspended: true, cycle_limit_bytes: 1024 ** 3, cycle_used_bytes: 1024 ** 3 })],
          [rule({ runtime_status: 'quota-paused' })],
        ),
    });
    const view = show({ canAuthorize: false });
    expect((await view.findAllByText('配额暂停')).length).toBeGreaterThan(0);
    expect(view.queryByRole('button', { name: '编辑 东京入口 的转发授权' })).toBeNull();
    expect(view.queryByRole('button', { name: '停止 东京入口 的转发' })).toBeNull();
  });

  it('接口尚未部署时只显示错误，不把失败误报成空授权', async () => {
    serve({ [`GET ${BASE}/forwarding`]: () => new Response('Not Found', { status: 404, statusText: 'Not Found' }) });
    const view = show({ canAuthorize: true });

    expect(await view.findAllByText('404 Not Found')).toHaveLength(2);
    expect(view.queryByText('尚未授权转发入口')).toBeNull();
    expect(view.queryByRole('button', { name: /新建规则/ })).toBeNull();
  });

  it('有效期暂停在机器摘要和规则上均有明确状态', async () => {
    serve({ [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule({ runtime_status: 'service-paused' })]) });
    const view = show();
    expect((await view.findAllByText('1 条有效期暂停')).length).toBeGreaterThan(0);
    expect(view.getAllByText('有效期暂停').length).toBeGreaterThan(0);
    expect(view.queryByText('1 条正常')).toBeNull();
  });

  it('管理员在原位表单选机器授权，使用完整的默认转发策略', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([], []),
      [`PUT ${BASE}/forwarding-nodes/n2`]: () =>
        overview([entry({ node_id: 'n2', node_name: '洛杉矶入口', rule_count: 0 })], []),
    });
    const view = show({ canAuthorize: true });

    await view.findByText('尚未授权转发入口');
    fireEvent.click(view.getByRole('button', { name: '授权机器' }));
    const dialog = view.getByRole('form', { name: '授权转发入口' });
    expect(view.queryByRole('dialog')).toBeNull();
    const submit = within(dialog).getByRole('button', { name: '授权' }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    fireEvent.click(within(dialog).getByRole('radio', { name: /洛杉矶入口/ }));
    fireEvent.click(submit);

    await waitFor(() => expect(writes(request)).toHaveLength(1));
    expect(writes(request)[0]).toEqual({
      path: `${BASE}/forwarding-nodes/n2`,
      method: 'PUT',
      body: {
        enabled: true,
        tcp_allowed: true,
        udp_allowed: true,
        listen_port_start: 30_000,
        listen_port_end: 59_999,
        max_rules: 100,
        allow_private_targets: false,
      },
    });
    expect(await view.findByRole('button', { name: '编辑 洛杉矶入口 的转发授权' })).toBeTruthy();
    expect(view.queryByRole('dialog')).toBeNull();
  });

  it('读数栏给出本期转发与已删除规则；IPv6 入口与目标不重复方括号', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      'GET /usage/user-cycles': () =>
        cycleUsage([
          usage(42, 3 * MiB, 1 * MiB),
          usage(7, 1 * MiB, 0),
          usage(42, 9 * MiB, 9 * MiB, { user_id: 'bob' }),
        ]),
    });
    const view = show();

    await view.findByText('现有规则');
    await waitFor(() => expect(view.container.querySelector('.user-usage-hero-value')?.textContent).toBe('5.0 MiB'));
    expect(view.getByText('其中 1.0 MiB 来自已删除的规则')).toBeTruthy();
    expect(view.container.querySelector('.fwd-cycle b')?.textContent).toBe('4.0 MiB');

    fireEvent.click(view.getByRole('button', { name: /现有规则，入口 20042/ }));
    expect(view.getByText('198.51.100.10:20042')).toBeTruthy();
    expect(view.getByText('[2001:4860:4860::8888]:20042')).toBeTruthy();
    expect(view.getAllByText('[2606:4700:4700::1111]:443').length).toBeGreaterThan(0);
    expect(view.container.textContent).not.toContain('[[');
  });

  it('按入口机器展示独立月流量和规则额度，计入已删除规则且不混入其他用户', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview(
          [entry(), entry({ node_id: 'n2', node_name: '洛杉矶入口', max_rules: 2 })],
          [rule(), rule({ id: 43, node_id: 'n2', enabled: false })],
        ),
      'GET /usage/user-cycles': () =>
        cycleUsage([
          usage(42, 3 * MiB, MiB),
          usage(7, MiB, 0),
          usage(43, 2 * MiB, 0, { node_id: 'n2' }),
          usage(42, 9 * MiB, 0, { user_id: 'bob' }),
        ]),
    });
    const view = show();
    const quotas = await view.findByRole('list', { name: '各转发入口的用量与规则额度' });
    await waitFor(() => expect(quotas.textContent).toContain('5.0 MiB'));
    expect(within(quotas).queryByText('入口机器')).toBeNull();
    const rows = within(quotas).getAllByRole('listitem');
    const tokyo = rows.find(row => row.textContent?.includes('东京入口'))!;
    const lax = rows.find(row => row.textContent?.includes('洛杉矶入口'))!;
    expect(tokyo.querySelector('.qta-figs')?.textContent).toBe('5.0 MiB');
    expect(tokyo.querySelector('.qta-left')?.textContent).toBe('规则1 / 4');
    expect(tokyo.querySelector('.qta-pct')?.textContent).toBe('25%');
    expect(lax.querySelector('.qta-figs')?.textContent).toBe('2.0 MiB');
    expect(lax.querySelector('.qta-left')?.textContent).toBe('规则1 / 2');
    expect(lax.querySelector('.qta-pct')?.textContent).toBe('50%');
  });

  it('月流量加载失败时保留未知读数，不伪装成零用量', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      'GET /usage/user-cycles': () => new Response('Unavailable', { status: 503 }),
    });
    const view = show();
    const quotas = await view.findByRole('list', { name: '各转发入口的用量与规则额度' });
    await waitFor(() => expect(view.container.querySelector('.user-usage-hero-value')?.textContent).toBe('—'));
    expect(quotas.querySelector('.qta-figs')?.textContent).toBe('—');
    expect(quotas.querySelector('.qta-left')?.textContent).toBe('规则1 / 4');
  });

  it('新建规则只填一栏目标，入口端口取下一个空闲端口', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`POST ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
    });
    const view = show();

    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: /新建规则/ }));
    expect((view.getByLabelText('入口端口') as HTMLInputElement).value).toBe('20000');
    fireEvent.change(view.getByLabelText('目标地址'), { target: { value: 'https://example.com:8443/path' } });
    fireEvent.click(view.getByRole('button', { name: '创建规则' }));

    await waitFor(() => expect(writes(request)).toHaveLength(1));
    expect(writes(request)[0].body).toEqual({
      node_id: 'n1',
      name: 'example.com:8443',
      protocol: 'tcp_udp',
      listen_port: 20_000,
      target_host: 'example.com',
      target_port: 8443,
      enabled: true,
    });
  });

  it('提交前提示端口冲突与私网目标，IPv6 目标按方括号形式提交', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`POST ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
    });
    const view = show();

    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: /新建规则/ }));
    const submit = view.getByRole('button', { name: '创建规则' }) as HTMLButtonElement;
    fireEvent.change(view.getByLabelText('目标地址'), { target: { value: 'example.com:80' } });
    fireEvent.change(view.getByLabelText('入口端口'), { target: { value: '20042' } });
    expect(view.getByRole('alert').textContent).toContain('已被「现有规则」占用');
    expect(view.getByRole('alert').textContent).toContain('下一个空闲端口 20000');
    expect(submit.disabled).toBe(true);

    fireEvent.change(view.getByLabelText('入口端口'), { target: { value: '20001' } });
    fireEvent.change(view.getByLabelText('目标地址'), { target: { value: '192.168.1.10:22' } });
    expect(view.getByRole('alert').textContent).toContain('未允许转发到私网');
    expect(submit.disabled).toBe(true);

    fireEvent.change(view.getByLabelText('目标地址'), { target: { value: '[2001:4860::1]:443' } });
    expect(view.queryByRole('alert')).toBeNull();
    fireEvent.click(submit);
    await waitFor(() => expect(writes(request)).toHaveLength(1));
    expect(writes(request)[0].body).toMatchObject({
      listen_port: 20_001,
      target_host: '[2001:4860::1]',
      target_port: 443,
    });
  });

  it('允许私网时仍在提交前拒绝回环和元数据目标', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry({ allow_private_targets: true })], [rule()]),
    });
    const view = show();
    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: /新建规则/ }));
    const submit = view.getByRole('button', { name: '创建规则' }) as HTMLButtonElement;
    for (const target of [
      '127.0.0.1:10085',
      '[::ffff:127.0.0.1]:10085',
      '169.254.169.254:80',
      '100.100.100.200:80',
      '[fd00:ec2::254]:80',
    ]) {
      fireEvent.change(view.getByLabelText('目标地址'), { target: { value: target } });
      expect(view.getByRole('alert').textContent).toContain('始终拒绝');
      expect(submit.disabled).toBe(true);
    }
    fireEvent.change(view.getByLabelText('目标地址'), { target: { value: '192.168.1.10:22' } });
    expect(view.queryByRole('alert')).toBeNull();
    expect(submit.disabled).toBe(false);
    expect(writes(request)).toHaveLength(0);
  });

  it('就地修改已有规则的协议和端口', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`PUT ${BASE}/forwarding/42`]: () => overview([entry()], [rule()]),
    });
    const view = show();

    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 :20042' }));
    const form = view.getByRole('form', { name: /编辑入口/ });
    fireEvent.click(within(form).getByRole('button', { name: 'UDP' }));
    fireEvent.change(within(form).getByLabelText('入口端口'), { target: { value: '20044' } });
    fireEvent.click(within(form).getByRole('button', { name: '保存' }));

    await waitFor(() => expect(writes(request)).toHaveLength(1));
    expect(writes(request)[0]).toEqual({
      path: `${BASE}/forwarding/42`,
      method: 'PUT',
      body: {
        node_id: 'n1',
        name: '现有规则',
        protocol: 'udp',
        listen_port: 20_044,
        target_host: '[2606:4700:4700::1111]',
        target_port: 443,
        enabled: true,
      },
    });
  });

  it('删除规则先在对话框里确认，不用浏览器原生确认框', async () => {
    const native = vi.spyOn(window, 'confirm');
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`DELETE ${BASE}/forwarding/42`]: () => overview([entry({ rule_count: 0 })], []),
    });
    const view = show();

    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: '东京入口 :20042 的更多操作' }));
    fireEvent.click(view.getByRole('menuitem', { name: /删除规则/ }));
    const dialog = view.getByRole('dialog', { name: '删除转发规则' });
    expect(within(dialog).getByText(/东京入口 立即停止监听 20042/)).toBeTruthy();
    fireEvent.click(within(dialog).getByRole('button', { name: '删除规则' }));

    await waitFor(() =>
      expect(writes(request)).toEqual([{ path: `${BASE}/forwarding/42`, method: 'DELETE', body: undefined }]),
    );
    expect(await view.findByText('还没有转发规则')).toBeTruthy();
    expect(native).not.toHaveBeenCalled();
  });

  it('confirms stop in the editor footer and writes only the current machine enabled=false', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`PUT ${BASE}/forwarding-nodes/n1`]: () =>
        overview([entry({ enabled: false })], [rule({ runtime_status: 'disabled' })]),
    });
    const view = show({ canAuthorize: true });
    fireEvent.click(await view.findByRole('button', { name: '编辑 东京入口 的转发授权' }));
    const form = view.getByRole('form');
    fireEvent.click(within(form).getByRole('button', { name: '停止转发' }));
    expect(view.queryByRole('dialog')).toBeNull();
    expect(writes(request)).toHaveLength(0);
    fireEvent.click(within(form).getByRole('button', { name: '停止转发' }));
    await waitFor(() => expect(writes(request)).toHaveLength(1));
    expect(writes(request)[0]).toEqual({
      path: `${BASE}/forwarding-nodes/n1`,
      method: 'PUT',
      body: {
        enabled: false,
        tcp_allowed: true,
        udp_allowed: true,
        listen_port_start: 20000,
        listen_port_end: 20100,
        max_rules: 4,
        allow_private_targets: false,
      },
    });
    await waitFor(() => expect(view.queryByRole('form')).toBeNull());
    expect(view.getByRole('button', { name: '恢复 东京入口 的转发' }).textContent).toBe('恢复');
  });

  it('confirms removal inline for a stopped entry and its rules', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry({ enabled: false })], [rule({ runtime_status: 'disabled' })]),
      [`DELETE ${BASE}/forwarding-nodes/n1`]: () => overview([], []),
    });
    const view = show({ canAuthorize: true });
    fireEvent.click(await view.findByRole('button', { name: '编辑 东京入口 的转发授权' }));
    const form = view.getByRole('form');
    fireEvent.click(within(form).getByRole('button', { name: '移除入口' }));
    expect(within(form).getByRole('alert').textContent).toContain('连同 1 条规则一起移除');
    expect(writes(request)).toHaveLength(0);
    fireEvent.click(within(form).getByRole('button', { name: '移除入口' }));
    await waitFor(() => expect(writes(request)[0]?.method).toBe('DELETE'));
    await view.findByText('尚未授权转发入口');
    expect(view.queryByText('已停止', { exact: true })).toBeNull();
  });

  it('修改授权：范围会排除启用中的规则时先提示，合规后保存策略', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`PUT ${BASE}/forwarding-nodes/n1`]: () => overview([entry()], [rule()]),
    });
    const view = show({ canAuthorize: true });

    await view.findByText('现有规则');
    fireEvent.click(view.getByRole('button', { name: '编辑 东京入口 的转发授权' }));
    const dialog = view.getByRole('form', { name: '编辑 东京入口 的转发授权' });
    expect(view.queryByRole('dialog')).toBeNull();
    expect(dialog.closest('.fwe-sheet')).toBeTruthy();
    const save = within(dialog).getByRole('button', { name: '保存' }) as HTMLButtonElement;

    fireEvent.change(within(dialog).getByLabelText('端口起始'), { target: { value: '20050' } });
    expect(within(dialog).getByText(/1 条启用中的规则不在新范围内（20042）/)).toBeTruthy();
    expect(save.disabled).toBe(true);

    fireEvent.change(within(dialog).getByLabelText('端口起始'), { target: { value: '20000' } });
    fireEvent.change(within(dialog).getByLabelText('端口结束'), { target: { value: '20999' } });
    fireEvent.change(within(dialog).getByLabelText('规则上限'), { target: { value: '8' } });
    fireEvent.click(within(dialog).getByRole('button', { name: '只 TCP' }));
    fireEvent.click(save);

    await waitFor(() => expect(writes(request)).toHaveLength(1));
    expect(writes(request)[0].body).toEqual({
      enabled: true,
      tcp_allowed: true,
      udp_allowed: false,
      listen_port_start: 20_000,
      listen_port_end: 20_999,
      max_rules: 8,
      allow_private_targets: false,
    });
  });

  it('只读身份可以展开详情，看不到授权与改规则的操作', async () => {
    serve({ [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]) });
    const view = show({ canManageRules: false });

    await view.findByText('现有规则');
    expect(view.queryByRole('button', { name: /新建规则/ })).toBeNull();
    expect(view.queryByRole('button', { name: '编辑 东京入口 :20042' })).toBeNull();
    expect(view.queryByRole('button', { name: '编辑 东京入口 的转发授权' })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: /现有规则，入口 20042/ }));
    expect(view.getByText('198.51.100.10:20042')).toBeTruthy();
    expect(view.queryByRole('button', { name: /删除规则/ })).toBeNull();
  });

  it('renders masked forwarding ports and destinations without copy, probe or write actions', async () => {
    const request = serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview(
          [
            entry({
              public_ipv4: '198.51.***.***',
              public_ipv6: '2001:4860:***',
              listen_port_start: '***',
              listen_port_end: '***',
            }),
          ],
          [
            rule({ name: '', listen_port: '***', target_host: '***.net', target_port: '***' }),
            rule({ id: 43, name: '', listen_port: '***', target_host: '[2001:db8:***]', target_port: '***' }),
          ],
        ),
    });
    const view = show({ canManageRules: false });
    const row = await view.findByRole('button', { name: '入口 *** 转发到 ***.net:***' });
    expect(view.getByRole('button', { name: '入口 *** 转发到 [2001:db8:***]:***' })).toBeTruthy();
    fireEvent.click(row);
    expect(view.getByText('198.51.***.***:***')).toBeTruthy();
    expect(view.getByText('[2001:4860:***]:***')).toBeTruthy();
    fireEvent.change(view.getByRole('combobox', { name: '规则排序' }), { target: { value: 'port' } });
    expect(view.queryByRole('button', { name: /复制|新建规则|编辑|更多操作|TCP Ping/ })).toBeNull();
    expect(view.container.textContent).not.toMatch(/NaN|undefined/);
    expect(request.mock.calls.some(([input]) => /tcp-pings|quota\/jobs|realtime/.test(String(input)))).toBe(false);
    expect(writes(request)).toEqual([]);
  });

  it('按机器筛选、搜索，并可按本期流量排序', async () => {
    serve({
      [`GET ${BASE}/forwarding`]: () =>
        overview(
          [entry({ rule_count: 2 }), entry({ node_id: 'n2', node_name: '洛杉矶入口', rule_count: 1 })],
          [
            rule(),
            rule({
              id: 43,
              node_id: 'n2',
              name: '游戏',
              listen_port: 20_001,
              target_host: 'game.example.org',
              target_port: 7777,
            }),
            rule({
              id: 44,
              name: 'b.example.com:80',
              listen_port: 20_002,
              target_host: 'b.example.com',
              target_port: 80,
            }),
          ],
        ),
      'GET /usage/user-cycles': () =>
        cycleUsage([usage(42, 4 * MiB, 0), usage(43, 9 * MiB, 0, { node_id: 'n2' }), usage(44, 1 * MiB, 0)]),
    });
    const view = show();
    const ports = () =>
      [...view.container.querySelectorAll('.fwd-port')].map(port => /:(\d+)/.exec(port.textContent ?? '')?.[1]);

    await view.findByText('游戏');
    expect(ports()).toEqual(['20002', '20042', '20001']);
    await waitFor(() => expect(view.container.querySelector('.user-usage-hero-value')?.textContent).toBe('14.0 MiB'));

    fireEvent.change(view.getByRole('combobox', { name: '规则排序' }), { target: { value: 'cycle' } });
    expect(ports()).toEqual(['20042', '20002', '20001']);

    fireEvent.change(view.getByRole('combobox', { name: '筛选转发机器' }), { target: { value: 'n2' } });
    expect(ports()).toEqual(['20001']);
    fireEvent.change(view.getByRole('combobox', { name: '筛选转发机器' }), { target: { value: '' } });
    expect(ports()).toHaveLength(3);

    fireEvent.change(view.getByLabelText('搜索规则'), { target: { value: 'example.org' } });
    expect(ports()).toEqual(['20001']);
  });

  it('能看实时的身份显示每条规则的当前速率；其他身份不订阅', async () => {
    const now = Date.now();
    const request = serve({
      [`GET ${BASE}/forwarding`]: () => overview([entry()], [rule()]),
      [`GET ${BASE}/realtime/snapshot`]: () => ({
        policy: { enabled: true },
        nodes: [
          {
            node_id: 'n1',
            connected: true,
            active: true,
            samples: [
              {
                node_id: 'n1',
                received_at_unix_millis: now,
                sample: {
                  sampled_at_unix_millis: now,
                  user_traffic: {
                    state: 'ready',
                    all_users: false,
                    forwarding_supported: true,
                    forwarding_expected: true,
                    users: [
                      {
                        tenant_id: 'platform.acme',
                        user_id: 'alice',
                        forwarding_rule_id: 42,
                        downlink_bytes_per_sec: 250_000,
                        uplink_bytes_per_sec: 12_500,
                      },
                    ],
                  },
                },
              },
            ],
          },
        ],
      }),
    });
    const view = show({ canViewRealtime: true });

    await view.findByText('1 条在转发');
    expect(view.getAllByText('2.10 Mbit/s').length).toBe(2);
    cleanup();

    request.mockClear();
    const plain = show();
    await plain.findByText('现有规则');
    expect(plain.queryByText('当前')).toBeNull();
    expect(request.mock.calls.some(([input]) => String(input).includes('/realtime/'))).toBe(false);
  });
});
