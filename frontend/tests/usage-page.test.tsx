import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

// Most ledger tests use readonly access. Realtime cases also exercise the realtime tab's
// subscription lifecycle without starting external services.
const access = vi.hoisted(() => ({ allowed: false, operatorId: 'readonly-test' }));
vi.mock('../src/session', () => ({
  canViewUserRealtimeTraffic: () => access.allowed,
  isVisitor: (who: { operator_id: string; role: string }) => who.operator_id === 'public' || who.role === 'user',
  useSession: () => ({
    who: {
      operator_id: access.operatorId,
      role: access.allowed ? 'system-admin' : 'readonly',
      tenant_scope: null,
      token_prefix: null,
      masked_assets: false,
    },
  }),
}));

let UsagePane: typeof import('../src/panes/usage').UsagePane;

beforeAll(async () => {
  window.matchMedia = ((media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ UsagePane } = await import('../src/panes/usage'));
});

afterEach(() => {
  cleanup();
  access.allowed = false;
  access.operatorId = 'readonly-test';
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

/** Freeze only the clock the daily average reads; query retries and waitFor keep real timers. */
function todayOnControlPlane(instant: string) {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(new Date(instant));
}

const readingOf = (ledger: HTMLElement, label: string) =>
  within(ledger).getByText(label, { selector: 'dt' }).parentElement?.querySelector('dd')?.textContent;

function client() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
  });
}

function json(body: unknown) {
  return new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
}

describe('usage realtime loading', () => {
  it.each(['/realtime/fleet-traffic/snapshot', '/nodes/agent-state', '/users?include_disabled=false'])(
    'only waits for the realtime snapshot while %s is pending',
    async pendingPath => {
      let finish!: (response: Response) => void;
      const pending = new Promise<Response>(resolve => {
        finish = resolve;
      });
      const responses: Record<string, unknown> = {
        '/realtime/fleet-traffic/snapshot': { policy: { enabled: true }, nodes: [], expected: [] },
        '/nodes/agent-state': { nodes: [] },
        '/users?include_disabled=false': { users: [] },
      };
      vi.stubGlobal('EventSource', undefined);
      vi.stubGlobal(
        'fetch',
        vi.fn(async (input: RequestInfo | URL) => {
          const path = String(input);
          if (path === pendingPath) return pending;
          if (path in responses) return json(responses[path]);
          throw new Error(`unexpected request: ${path}`);
        }),
      );
      const view = render(
        <QueryClientProvider client={client()}>
          <UsagePane />
        </QueryClientProvider>,
      );
      const paper = view.container.querySelector('.nd-paper');
      const panel = view.getByRole('tabpanel', { name: '实时' });
      if (pendingPath === '/realtime/fleet-traffic/snapshot') {
        expect(within(panel).getByRole('status', { name: '读取实时用量' })).toBeTruthy();
        expect(view.queryByLabelText('机队当前速率')).toBeNull();
        expect(view.container.querySelector('.usage-tab-loading')?.getAttribute('aria-busy')).toBe('true');
      } else {
        await view.findByLabelText('机队当前速率');
        expect(within(panel).queryByRole('status', { name: '读取实时用量' })).toBeNull();
      }
      const surface = view.container.querySelector('.flt-panel');

      await act(async () => finish(json(responses[pendingPath])));

      await view.findByLabelText('机队当前速率');
      expect(view.queryByRole('status', { name: '读取实时用量' })).toBeNull();
      expect(view.container.querySelector('.nd-paper')).toBe(paper);
      expect(view.getByRole('tabpanel', { name: '实时' })).toBe(panel);
      if (surface) expect(view.container.querySelector('.flt-panel')).toBe(surface);
    },
  );

  it('replaces realtime progress with an error when the first reading fails', async () => {
    vi.stubGlobal('EventSource', undefined);
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/nodes/agent-state') return json({ nodes: [] });
        if (path === '/users?include_disabled=false') return json({ users: [] });
        if (path === '/realtime/fleet-traffic/snapshot') return new Response('{}', { status: 503 });
        throw new Error(`unexpected request: ${path}`);
      }),
    );
    const view = render(
      <QueryClientProvider client={client()}>
        <UsagePane />
      </QueryClientProvider>,
    );

    await view.findByText('无法读取机队实时流量，正在等待服务恢复');
    expect(view.queryByRole('status', { name: '读取实时用量' })).toBeNull();
    expect(view.getByRole('heading', { name: '用量' })).toBeTruthy();
  });
});

describe('usage month view', () => {
  it('keeps the usage paper mounted while the first monthly reading loads', async () => {
    let finishMonthly: ((response: Response) => void) | undefined;
    const monthlyResponse = new Promise<Response>(resolve => {
      finishMonthly = resolve;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/usage/monthly-summary') return monthlyResponse;
        return Promise.reject(new Error(`unexpected request: ${path}`));
      }),
    );

    const view = render(
      <QueryClientProvider client={client()}>
        <UsagePane />
      </QueryClientProvider>,
    );
    const page = view.container.querySelector('.usage-page');
    const paper = view.container.querySelector('.nd-paper');
    expect(page).toBeTruthy();
    expect(paper).toBeTruthy();

    fireEvent.click(view.getByRole('tab', { name: '汇总' }));

    const panel = view.getByRole('tabpanel', { name: '汇总' });
    expect(view.container.querySelector('.usage-page')).toBe(page);
    expect(view.container.querySelector('.nd-paper')).toBe(paper);
    expect(panel.getAttribute('aria-busy')).toBe('true');
    expect(within(panel).getByText('读取月度用量')).toBeTruthy();
    expect(view.container.querySelector('.loading-state')).toBeNull();

    await act(async () => {
      finishMonthly?.(
        json({
          month_start: '2026-09-01 00:00:00',
          month_end: '2026-10-01 00:00:00',
          views: [],
          days: [{ day: '2026-09-01', uplink_bytes: 1024, downlink_bytes: 2048, has_gap: false }],
        }),
      );
      await monthlyResponse;
    });

    await view.findByLabelText('2026 年 9 月用量概览');
    expect(view.container.querySelector('.usage-page')).toBe(page);
    expect(view.container.querySelector('.nd-paper')).toBe(paper);
    expect(view.getByRole('tabpanel', { name: '汇总' })).toBe(panel);
    expect(panel.hasAttribute('aria-busy')).toBe(false);
    expect(within(panel).queryByText('读取月度用量')).toBeNull();
  });

  it('includes forwarding-only users and distinguishes source totals without inventing a line', async () => {
    const queryClient = client();
    queryClient.setQueryData(['usage-monthly'], {
      month_start: '2026-09-01 00:00:00',
      month_end: '2026-10-01 00:00:00',
      views: [],
      forwarding: [
        {
          tenant_id: 'platform',
          user_id: 'alice',
          node_id: 'n1',
          rule_id: 7,
          rule_name: 'Web',
          uplink_bytes: 1024,
          downlink_bytes: 2048,
          has_gap: false,
        },
      ],
      days: [{ day: '2026-09-01', uplink_bytes: 1024, downlink_bytes: 2048, has_gap: false }],
    });
    const view = render(
      <QueryClientProvider client={queryClient}>
        <UsagePane />
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('tab', { name: '汇总' }));
    const ledger = view.getByLabelText('2026 年 9 月用量概览');
    expect(ledger.querySelector('.usage-hero-value')?.textContent).toBe('3 KiB');
    expect(readingOf(ledger, '接入')).toBe('0 B');
    expect(readingOf(ledger, '转发')).toBe('3 KiB');
    expect(readingOf(ledger, '活跃用户')).toBe('1');
    expect(readingOf(ledger, '线路')).toBe('0');
  });
  it('keeps the mounted panel and current reading visible while another month loads', async () => {
    let finishPreviousMonth: ((response: Response) => void) | undefined;
    const previousMonth = new Promise<Response>(resolve => {
      finishPreviousMonth = resolve;
    });
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/usage/monthly-summary') {
          return Promise.resolve(
            json({
              month_start: '2026-09-01 00:00:00',
              month_end: '2026-10-01 00:00:00',
              views: [],
              days: [{ day: '2026-09-01', uplink_bytes: 1024, downlink_bytes: 2048, has_gap: false }],
            }),
          );
        }
        if (path === '/usage/monthly-summary?month_offset=-1') return previousMonth;
        return Promise.reject(new Error(`unexpected request: ${path}`));
      }),
    );

    const view = render(
      <QueryClientProvider client={client()}>
        <UsagePane />
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('tab', { name: '汇总' }));
    await view.findByLabelText('2026 年 9 月用量概览');

    const panel = view.getByRole('tabpanel', { name: '汇总' });
    const cockpit = view.container.querySelector('.usage-cockpit');
    expect(panel).toBeTruthy();
    expect(cockpit).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: '上月' }));

    expect(view.getByRole('tabpanel', { name: '汇总' })).toBe(panel);
    expect(view.container.querySelector('.usage-cockpit')).toBe(cockpit);
    expect(view.getByLabelText('2026 年 9 月用量概览')).toBeTruthy();
    expect(panel?.getAttribute('aria-busy')).toBe('true');
    expect(view.getByText('读取中')).toBeTruthy();

    await act(async () => {
      finishPreviousMonth?.(
        json({
          month_start: '2026-08-01 00:00:00',
          month_end: '2026-09-01 00:00:00',
          views: [],
          days: [{ day: '2026-08-01', uplink_bytes: 2048, downlink_bytes: 4096, has_gap: false }],
        }),
      );
      await previousMonth;
    });

    await view.findByLabelText('2026 年 8 月用量概览');
    expect(view.getByRole('tabpanel', { name: '汇总' })).toBe(panel);
    expect(view.container.querySelector('.usage-cockpit')).toBe(cockpit);
    expect(panel?.hasAttribute('aria-busy')).toBe(false);
  });

  it('shows one month at a time and requests the previous month when selected', async () => {
    // 12:00 on 2 September at +08: the 2nd is still accumulating.
    todayOnControlPlane('2026-09-02T04:00:00Z');
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/usage/monthly-summary') {
        return json({
          month_start: '2026-09-01 00:00:00',
          month_end: '2026-10-01 00:00:00',
          views: [
            {
              tenant_id: 'platform.acme',
              user_id: 'alice',
              app_id: 'tokyo',
              uplink_bytes: 1024,
              downlink_bytes: 3072,
              has_gap: false,
            },
          ],
          days: [
            { day: '2026-09-01', uplink_bytes: 1024, downlink_bytes: 2048, has_gap: false },
            { day: '2026-09-02', uplink_bytes: 0, downlink_bytes: 1024, has_gap: false },
          ],
        });
      }
      if (path === '/usage/monthly-summary?month_offset=-1') {
        return json({
          month_start: '2026-08-01 00:00:00',
          month_end: '2026-09-01 00:00:00',
          views: [
            {
              tenant_id: 'platform.acme',
              user_id: 'alice',
              app_id: 'tokyo',
              uplink_bytes: 2048,
              downlink_bytes: 4096,
              has_gap: false,
            },
          ],
          days: [{ day: '2026-08-01', uplink_bytes: 2048, downlink_bytes: 4096, has_gap: false }],
        });
      }
      throw new Error(`unexpected request: ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const queryClient = client();
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [{ id: 'tokyo', label: '东京入口' }] } });
    queryClient.setQueryData(['nodes'], { nodes: [] });

    const view = render(
      <QueryClientProvider client={queryClient}>
        <UsagePane />
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('tab', { name: '汇总' }));

    await waitFor(() =>
      expect(view.getByLabelText('2026 年 9 月用量概览').querySelector('.usage-hero-value')?.textContent).toBe('4 KiB'),
    );
    const title = view.getByRole('heading', { name: '用量' });
    expect(title.tagName).toBe('H1');
    expect(title.closest('.nd-page-head')?.querySelector('.cg-plate')).toBeTruthy();
    expect(title.parentElement?.textContent).not.toContain('2026 年 9 月');
    expect(view.getByRole('heading', { name: '每日流量' }).parentElement?.textContent).not.toContain('2026 年 9 月');
    expect(view.container.querySelectorAll('.nd-paper')).toHaveLength(1);
    expect(view.container.querySelector('.flt-panel')).toBeNull();
    expect(view.getByRole('heading', { name: '每日流量' }).closest('[role="tabpanel"]')).toBe(
      view.getByRole('tabpanel', { name: '汇总' }),
    );

    const ledger = view.getByLabelText('2026 年 9 月用量概览');
    expect(ledger.querySelector('.usage-period')?.textContent).toBe('2026 年 9 月 1 日 – 30 日');
    expect(readingOf(ledger, '上行')).toBe('1 KiB');
    expect(readingOf(ledger, '下行')).toBe('3 KiB');
    expect([...ledger.querySelectorAll('.usage-share')].map(share => share.textContent)).toEqual(['25.0%', '75.0%']);
    // The average covers only 1 September; today's partial traffic stays out of it.
    expect(readingOf(ledger, '日均')).toBe('3 KiB');
    expect(within(ledger).getByText('日均', { selector: 'dt' }).nextElementSibling?.getAttribute('title')).toBe(
      '按 1 个完整日计算',
    );
    expect(readingOf(ledger, '活跃用户')).toBe('1');
    expect(readingOf(ledger, '线路')).toBe('1');
    expect(view.queryByRole('heading', { name: '流量明细' })).toBeNull();
    expect(view.container.querySelector('.usage-breakdown')).toBeNull();
    expect(view.queryByText('原始样本')).toBeNull();
    expect(view.container.querySelector('.usage-raw')).toBeNull();
    expect(view.getByRole('button', { name: '本月' }).getAttribute('aria-pressed')).toBe('true');
    const septemberFirst = view.getByRole('button', {
      name: /查看 2026-09-01：上行 1 KiB，下行 2 KiB/,
    });
    const septemberSecond = view.getByRole('button', {
      name: /查看 2026-09-02：上行 0 B，下行 1 KiB/,
    });
    expect(septemberSecond.getAttribute('aria-pressed')).toBe('true');
    expect(view.getByLabelText('2026-09-02 流量详情')).toBeTruthy();

    // The ledger's peak day selects the same column as clicking it in the chart.
    fireEvent.click(within(ledger).getByRole('button', { name: '在每日流量中查看 9 月 1 日' }));
    expect(septemberFirst.getAttribute('aria-pressed')).toBe('true');
    expect(septemberSecond.getAttribute('aria-pressed')).toBe('false');
    expect(view.getByLabelText('2026-09-01 流量详情')).toBeTruthy();

    fireEvent.focus(septemberFirst);
    const firstDayDetail = view.getByLabelText('2026-09-01 流量详情');
    expect(within(firstDayDetail).getByText('3 KiB')).toBeTruthy();
    expect(septemberFirst.getAttribute('aria-pressed')).toBe('true');

    fireEvent.click(septemberSecond);
    const secondDayDetail = view.getByLabelText('2026-09-02 流量详情');
    expect(secondDayDetail.querySelector('.up dd')?.textContent).toBe('0 B');
    expect(secondDayDetail.querySelector('.down dd')?.textContent).toBe('1 KiB');
    expect(secondDayDetail.querySelector('dl > div:last-child dd')?.textContent).toBe('1 KiB');

    fireEvent.click(view.getByRole('button', { name: '上月' }));

    await waitFor(() =>
      expect(view.getByLabelText('2026 年 8 月用量概览').querySelector('.usage-hero-value')?.textContent).toBe('6 KiB'),
    );
    const august = view.getByLabelText('2026 年 8 月用量概览');
    // A finished month averages over all 31 days: 6 KiB / 31.
    expect(readingOf(august, '日均')).toBe('198 B');
    expect(within(august).getByText('日均', { selector: 'dt' }).nextElementSibling?.getAttribute('title')).toBe(
      '按 31 个完整日计算',
    );
    expect(within(august).getByRole('button', { name: '在每日流量中查看 8 月 1 日' })).toBeTruthy();
    expect(view.getByRole('button', { name: '上月' }).getAttribute('aria-pressed')).toBe('true');
    expect(view.getByRole('heading', { name: '用量' }).parentElement?.textContent).not.toContain('2026 年 8 月');
    expect(view.getByRole('heading', { name: '每日流量' }).parentElement?.textContent).not.toContain('2026 年 8 月');
    expect(view.getByRole('button', { name: /查看 2026-08-01：上行 2 KiB，下行 4 KiB/ })).toBeTruthy();
    expect(view.getByLabelText('2026-08-01 流量详情')).toBeTruthy();
    expect(
      [...view.getByRole('list', { name: '2026 年 8 月每日流量' }).querySelectorAll('.usage-day-label.major')].map(
        label => label.textContent,
      ),
    ).toEqual(['1', '5', '10', '15', '20', '25', '31']);
    expect(
      fetchMock.mock.calls.map(call => String(call[0])).filter(path => path.startsWith('/usage/monthly-summary')),
    ).toEqual(['/usage/monthly-summary', '/usage/monthly-summary?month_offset=-1']);
  });

  it('shows reported totals without repeating collection gaps in summaries, columns or day details', () => {
    const queryClient = client();
    queryClient.setQueryData(['usage-monthly'], {
      month_start: '2026-09-01 00:00:00',
      month_end: '2026-10-01 00:00:00',
      views: [
        {
          tenant_id: 'platform.acme',
          user_id: 'alice',
          app_id: 'tokyo',
          uplink_bytes: 1024,
          downlink_bytes: 2048,
          has_gap: true,
        },
      ],
      days: [
        { day: '2026-09-01', uplink_bytes: 1024, downlink_bytes: 2048, has_gap: true },
        { day: '2026-09-02', uplink_bytes: 0, downlink_bytes: 0, has_gap: true },
      ],
    });

    const view = render(
      <QueryClientProvider client={queryClient}>
        <UsagePane />
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('tab', { name: '汇总' }));

    expect(view.getByLabelText('2026 年 9 月用量概览').querySelector('.usage-hero-value')?.textContent).toBe('3 KiB');
    expect(view.queryByRole('status')).toBeNull();
    expect(view.container.querySelector('.usage-gap, .usage-gap-mark, .usage-day-track .gap')).toBeNull();
    expect(view.container.innerHTML).not.toContain('采集缺口');

    const emptyDay = view.getByRole('button', { name: '查看 2026-09-02：上行 0 B，下行 0 B' });
    expect(emptyDay.getAttribute('aria-pressed')).toBe('true');
    expect(view.getByLabelText('2026-09-02 流量详情').querySelector('dl > div:last-child dd')?.textContent).toBe('0 B');

    const firstDay = view.getByRole('button', { name: '查看 2026-09-01：上行 1 KiB，下行 2 KiB' });
    expect(firstDay.title).toBe('2026-09-01：上行 1 KiB，下行 2 KiB');
    fireEvent.click(firstDay);
    const detail = view.getByLabelText('2026-09-01 流量详情');
    expect(detail.querySelector('.up dd')?.textContent).toBe('1 KiB');
    expect(detail.querySelector('.down dd')?.textContent).toBe('2 KiB');
    expect(detail.querySelector('dl > div:last-child dd')?.textContent).toBe('3 KiB');
    expect(within(detail).getByText('点按柱形切换日期')).toBeTruthy();
    expect(view.container.innerHTML).not.toContain('采集缺口');
  });

  it('keeps the page usable while an older control plane omits daily rows', () => {
    const queryClient = client();
    queryClient.setQueryData(['usage-monthly'], {
      month_start: '2026-09-01 00:00:00',
      month_end: '2026-10-01 00:00:00',
      views: [],
    });
    queryClient.setQueryData(['snapshot'], { snapshot: { apps: [] } });

    const view = render(
      <QueryClientProvider client={queryClient}>
        <UsagePane />
      </QueryClientProvider>,
    );
    fireEvent.click(view.getByRole('tab', { name: '汇总' }));

    expect(view.getByRole('heading', { name: '用量' })).toBeTruthy();
    expect(view.getByText('当前控制面尚未提供每日流量。')).toBeTruthy();
    // Without daily rows the ledger cannot tell which days ended or which day peaked.
    const ledger = view.getByLabelText('2026 年 9 月用量概览');
    expect(readingOf(ledger, '日均')).toBe('—');
    expect(readingOf(ledger, '峰值日')).toBe('—');
    expect(within(ledger).queryByRole('button')).toBeNull();
  });
});

describe('usage isolation', () => {
  const monthly = {
    month_start: '2026-10-01 00:00:00',
    month_end: '2026-11-01 00:00:00',
    views: [],
    days: [],
  };

  it('shares the release page shell and preserves the month and day across keyboard tab changes', async () => {
    access.allowed = true;
    class FleetStream extends EventTarget {
      close = vi.fn();
    }
    vi.stubGlobal('EventSource', FleetStream);
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        if (path === '/nodes/agent-state') return json({ nodes: [] });
        if (path === '/users?include_disabled=false') return json({ users: [] });
        if (path === '/realtime/fleet-traffic/snapshot')
          return json({ policy: { enabled: true }, nodes: [], expected: [] });
        throw new Error(`unexpected request: ${path}`);
      }),
    );
    const queryClient = client();
    queryClient.setQueryData(['usage-monthly'], monthly);
    queryClient.setQueryData(['usage-monthly', -1], {
      ...monthly,
      month_start: '2026-09-01 00:00:00',
      days: [
        { day: '2026-09-01', uplink_bytes: 1024, downlink_bytes: 1024, has_gap: false },
        { day: '2026-09-02', uplink_bytes: 1024, downlink_bytes: 2048, has_gap: false },
      ],
    });
    const view = render(
      <QueryClientProvider client={queryClient}>
        <UsagePane />
      </QueryClientProvider>,
    );
    const tabs = within(view.getByRole('tablist', { name: '用量页签' })).getAllByRole('tab');
    expect(tabs.map(tab => tab.textContent)).toEqual(['实时', '汇总']);
    expect(view.container.querySelectorAll('.nd-sheet > .nd-paper > .nd-page-head')).toHaveLength(1);
    expect(view.container.querySelector('.panel')).toBeNull();
    expect(tabs[0].getAttribute('aria-selected')).toBe('true');
    const panelId = view.getByRole('tabpanel', { name: '实时' }).id;
    expect(tabs.every(tab => tab.getAttribute('aria-controls') === panelId)).toBe(true);

    tabs[0].focus();
    fireEvent.keyDown(tabs[0], { key: 'ArrowRight' });
    await waitFor(() => expect(document.activeElement).toBe(tabs[1]));
    expect(tabs[1].getAttribute('aria-selected')).toBe('true');
    expect(tabs[0].tabIndex).toBe(-1);
    expect(view.getByRole('tabpanel', { name: '汇总' }).getAttribute('aria-labelledby')).toBe(tabs[1].id);

    fireEvent.click(view.getByRole('button', { name: '上月' }));
    await view.findByLabelText('2026 年 9 月用量概览');
    fireEvent.click(view.getByRole('button', { name: /查看 2026-09-01：/ }));
    expect(view.getByLabelText('2026-09-01 流量详情')).toBeTruthy();

    fireEvent.keyDown(tabs[1], { key: 'ArrowRight' });
    await waitFor(() => expect(document.activeElement).toBe(tabs[0]));
    expect(view.getByRole('tabpanel', { name: '实时' }).getAttribute('aria-labelledby')).toBe(tabs[0].id);
    expect(view.queryByRole('group', { name: '用量月份' })).toBeNull();
    expect(view.queryByLabelText('2026 年 9 月用量概览')).toBeNull();

    fireEvent.keyDown(tabs[0], { key: 'ArrowRight' });
    await waitFor(() => expect(document.activeElement).toBe(tabs[1]));
    expect(view.getByRole('button', { name: '上月' }).getAttribute('aria-pressed')).toBe('true');
    expect(view.getByLabelText('2026-09-01 流量详情')).toBeTruthy();
    fireEvent.keyDown(tabs[1], { key: 'Home' });
    await waitFor(() => expect(document.activeElement).toBe(tabs[0]));
    fireEvent.keyDown(tabs[0], { key: 'End' });
    await waitFor(() => expect(document.activeElement).toBe(tabs[1]));
    fireEvent.keyDown(tabs[1], { key: 'ArrowLeft' });
    await waitFor(() => expect(document.activeElement).toBe(tabs[0]));
  });

  it('starts the fleet subscription in the default realtime tab and closes it on summary', async () => {
    access.allowed = true;
    const streams: { url: string; close: ReturnType<typeof vi.fn> }[] = [];
    class FleetStream extends EventTarget {
      url: string;
      close = vi.fn();
      constructor(url: string) {
        super();
        this.url = url;
        streams.push(this);
      }
    }
    vi.stubGlobal('EventSource', FleetStream);
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/nodes/agent-state') return json({ nodes: [] });
      if (path === '/users?include_disabled=false') return json({ users: [] });
      if (path === '/realtime/fleet-traffic/snapshot')
        return json({ policy: { enabled: true }, nodes: [], expected: [] });
      throw new Error(`unexpected request: ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const queryClient = client();
    queryClient.setQueryData(['usage-monthly'], monthly);
    const view = render(
      <QueryClientProvider client={queryClient}>
        <UsagePane />
      </QueryClientProvider>,
    );
    await view.findByLabelText('机队当前速率');
    expect(streams).toHaveLength(1);
    expect(streams[0].url).toBe('/realtime/fleet-traffic/events');
    expect(streams[0].close).not.toHaveBeenCalled();

    fireEvent.click(view.getByRole('tab', { name: '汇总' }));
    expect(streams[0].close).toHaveBeenCalledOnce();
    expect(view.queryByLabelText('机队实时流量')).toBeNull();
    expect(view.getByRole('tabpanel', { name: '汇总' })).toBeTruthy();
  });

  it('shows visitors the realtime matrix while withholding every detail action', async () => {
    access.operatorId = 'public';
    const now = Date.now();
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) === '/usage/monthly-summary') return json(monthly);
      if (String(input) === '/nodes/agent-state')
        return json({ nodes: [{ node_id: 'hk', name: '香港 HKIX', lifecycle_phase: 'active' }] });
      if (String(input) === '/users?include_disabled=false')
        return json({ users: [{ id: 'alice', tenant_id: 'platform', status: 'active', created_at: '' }] });
      if (String(input) === '/realtime/fleet-traffic/snapshot')
        return json({
          policy: { enabled: true },
          nodes: [
            {
              node_id: 'hk',
              connected: true,
              active: true,
              interval_secs: 1,
              samples: [
                {
                  node_id: 'hk',
                  received_at_unix_millis: now,
                  sample: {
                    sampled_at_unix_millis: now,
                    rx_bytes_per_sec: 100,
                    tx_bytes_per_sec: 50,
                    user_traffic: {
                      state: 'ready',
                      all_users: true,
                      users: [
                        {
                          tenant_id: 'platform',
                          user_id: 'alice',
                          downlink_bytes_per_sec: 80,
                          uplink_bytes_per_sec: 20,
                        },
                      ],
                    },
                  },
                },
              ],
            },
          ],
          expected: [{ node_id: 'hk', users: [{ tenant_id: 'platform', user_id: 'alice' }] }],
        });
      throw new Error(`unexpected request: ${String(input)}`);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = render(
      <QueryClientProvider client={client()}>
        <UsagePane />
      </QueryClientProvider>,
    );
    const matrix = within(await view.findByLabelText('用户与机器的当前速率')).getByRole('table');
    const tabs = within(view.getByRole('tablist', { name: '用量页签' })).getAllByRole('tab');
    expect(tabs.map(tab => tab.textContent)).toEqual(['实时', '汇总']);
    expect(tabs[0].getAttribute('aria-selected')).toBe('true');
    expect(within(matrix).getByRole('rowheader', { name: /alice/ })).toBeTruthy();
    expect(within(matrix).queryByRole('button', { name: /查看机器用量/ })).toBeNull();
    expect(view.queryByRole('combobox', { name: '选择机器查看用量' })).toBeNull();
    expect(fetchMock.mock.calls.some(([input]) => String(input) === '/users?include_disabled=false')).toBe(true);
    expect(fetchMock.mock.calls.some(([input]) => String(input) === '/realtime/fleet-traffic/snapshot')).toBe(true);

    fireEvent.click(view.getByRole('tab', { name: '汇总' }));
    await view.findByLabelText('2026 年 10 月用量概览');
    expect(view.queryByText('流量预测')).toBeNull();
    expect(view.queryByLabelText('机队实时流量')).toBeNull();
    expect(view.getByRole('tabpanel', { name: '汇总' })).toBeTruthy();
    expect(view.getByRole('group', { name: '用量月份' })).toBeTruthy();
  });
});
