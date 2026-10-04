import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

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

describe('usage month view', () => {
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
    await view.findByLabelText('2026 年 9 月用量概览');

    const panel = view.container.querySelector('.usage-summary-panel');
    const cockpit = view.container.querySelector('.usage-cockpit');
    expect(panel).toBeTruthy();
    expect(cockpit).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: '上月' }));

    expect(view.container.querySelector('.usage-summary-panel')).toBe(panel);
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
    expect(view.container.querySelector('.usage-summary-panel')).toBe(panel);
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

    await waitFor(() =>
      expect(view.getByLabelText('2026 年 9 月用量概览').querySelector('.usage-hero-value')?.textContent).toBe('4 KiB'),
    );
    const title = view.getByRole('heading', { name: '用量' });
    expect(title.closest('.panel.titled')).toBeTruthy();
    expect(title.parentElement?.querySelector('.list-ico')).toBeTruthy();
    expect(title.parentElement?.textContent).not.toContain('2026 年 9 月');
    expect(view.getByRole('heading', { name: '每日流量' }).parentElement?.textContent).not.toContain('2026 年 9 月');
    // One card: the ledger and the daily chart share the page title instead of a second panel.
    expect(view.container.querySelectorAll('.panel')).toHaveLength(1);
    expect(view.getByRole('heading', { name: '每日流量' }).closest('.panel')).toBe(title.closest('.panel'));

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
    expect(fetchMock.mock.calls.map(call => String(call[0]))).toEqual([
      '/usage/monthly-summary',
      '/usage/monthly-summary?month_offset=-1',
    ]);
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

    expect(view.getByRole('heading', { name: '用量' })).toBeTruthy();
    expect(view.getByText('当前控制面尚未提供每日流量。')).toBeTruthy();
    // Without daily rows the ledger cannot tell which days ended or which day peaked.
    const ledger = view.getByLabelText('2026 年 9 月用量概览');
    expect(readingOf(ledger, '日均')).toBe('—');
    expect(readingOf(ledger, '峰值日')).toBe('—');
    expect(within(ledger).queryByRole('button')).toBeNull();
  });
});
