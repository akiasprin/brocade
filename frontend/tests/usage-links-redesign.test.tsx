import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

vi.mock('../src/panes/fleet-net-panel', () => ({
  FleetNetPanel: () => <section data-testid="fleet-throughput">fleet throughput</section>,
}));

vi.mock('../src/panes/telemetry', () => ({
  HopLinkTable: ({ title }: { title?: string }) => (
    <section>
      <h4>{title}</h4>
      <div data-testid="hop-quality">hop quality</div>
    </section>
  ),
}));

let UsagePane: typeof import('../src/panes/usage').UsagePane;
let LinksPane: typeof import('../src/panes/links').LinksPane;

beforeAll(async () => {
  window.matchMedia = ((media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ UsagePane } = await import('../src/panes/usage'));
  ({ LinksPane } = await import('../src/panes/links'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function client() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
  });
}

function json(body: unknown) {
  return new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
}

describe('usage month view', () => {
  it('shows one month at a time and requests the previous month when selected', async () => {
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
      expect(view.getByLabelText('2026 年 9 月用量概览').querySelector('.usage-total strong')?.textContent).toBe(
        '4 KiB',
      ),
    );
    const title = view.getByRole('heading', { name: '用量' });
    expect(title.closest('.panel.titled')).toBeTruthy();
    expect(title.parentElement?.querySelector('.list-ico')).toBeTruthy();
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
      expect(view.getByLabelText('2026 年 8 月用量概览').querySelector('.usage-total strong')?.textContent).toBe(
        '6 KiB',
      ),
    );
    expect(view.getByRole('button', { name: '上月' }).getAttribute('aria-pressed')).toBe('true');
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
  });
});

describe('links overview', () => {
  it('organizes the existing probe sources into one status-first page', () => {
    const queryClient = client();
    queryClient.setQueryData(['nodes'], {
      nodes: [
        { node_id: 'n1', name: '台北一号' },
        { node_id: 'n2', name: '东京一号' },
      ],
    });
    queryClient.setQueryData(['link-mtu'], {
      default_mtu: 1420,
      nodes: [
        {
          node_id: 'n1',
          current_mtu: 1380,
          overridden: true,
          suggested_mtu: 1380,
          tightest_peer: 'n2',
          inconclusive: 0,
        },
      ],
      links: [
        {
          node_id: 'n1',
          peer_node_id: 'n2',
          endpoint_host: '203.0.113.2',
          status: 'ok',
          path_mtu: 1440,
          suggested_wg_mtu: 1380,
          probed_at: '2026-09-12T10:00:00+08:00',
        },
      ],
    });
    queryClient.setQueryData(['e2e-probes'], {
      chains: [
        {
          app_id: 'main',
          chain_id: 'taipei-tokyo',
          chain_name: '台北到东京',
          node_id: 'n1',
          status: 'ok',
          ttfb_ms: 42,
          exit_ip: '203.0.113.2',
          exit_loc: 'JP',
          exit_verdict: 'match',
          detail: null,
          probed_at: '2026-09-12T10:00:00+08:00',
          samples: { probed_at_unix_secs: [], status: [], ttfb_ms: [] },
        },
      ],
    });
    queryClient.setQueryData(['link-quality'], {
      hops: [
        {
          node_id: 'n1',
          cc_algo: 'bbr',
          sample: {
            chain_id: 'taipei-tokyo',
            peer_node_id: 'n2',
            window_start_unix_secs: 100,
            window_end_unix_secs: 130,
            conns: 2,
            conns_measured: 2,
            btlbw_p50_bps: 1000,
            btlbw_p90_bps: 1200,
            min_rtt_us: 1000,
            rtt_p50_us: 1200,
            rtt_p90_us: 1400,
            retrans_pct: 0,
            busy_pct: 10,
            rwnd_limited_pct: 0,
            sndbuf_limited_pct: 0,
          },
        },
      ],
    });

    const view = render(
      <QueryClientProvider client={queryClient}>
        <LinksPane />
      </QueryClientProvider>,
    );

    const title = view.getByRole('heading', { name: '链路与 MTU' });
    expect(title.closest('.panel.titled')).toBeTruthy();
    expect(title.parentElement?.querySelector('.list-ico')).toBeTruthy();
    expect(view.getByRole('status').textContent).toContain('当前未发现链路异常');
    expect(view.getByRole('heading', { name: '端到端探测' })).toBeTruthy();
    expect(view.getByRole('heading', { name: '逐跳质量' })).toBeTruthy();
    expect(view.getByRole('heading', { name: '机器 MTU' })).toBeTruthy();
    expect(view.getByText('台北到东京')).toBeTruthy();
    expect(view.getByTestId('fleet-throughput')).toBeTruthy();
    expect(view.getByTestId('hop-quality')).toBeTruthy();
  });
});
