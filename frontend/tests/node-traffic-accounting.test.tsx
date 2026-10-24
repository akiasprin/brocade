import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, NodeTrafficItem, NodeTrafficView, UsageNodeSeries } from '../src/api';

let exactBytes: typeof import('../src/panes/nodes').exactBytes;
let NicMonthTotal: typeof import('../src/panes/nodes').NicMonthTotal;
let trafficCalibrationBytes: typeof import('../src/panes/nodes').trafficCalibrationBytes;
let TrafficAccountingCard: typeof import('../src/panes/nodes').TrafficAccountingCard;
let usageRoleTimeline: typeof import('../src/panes/nodes').usageRoleTimeline;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
  ({ exactBytes, NicMonthTotal, trafficCalibrationBytes, TrafficAccountingCard, usageRoleTimeline } =
    await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const view: NodeTrafficView = {
  nodes: [
    {
      node_id: 'hk',
      tenant_id: 'platform',
      name: '香港',
      cycle_kind: 'monthly',
      accounting_mode: 'bidirectional_sum',
      reset_month: null,
      reset_day: 1,
      quota_bytes: null,
      period_start_unix_secs: 1_788_192_000,
      period_end_unix_secs: 1_790_870_400,
      rx_bytes: '1099511627776',
      tx_bytes: '549755813888',
      total_bytes: '1649267441664',
      interface: 'eth0',
      tracking_started_at_unix_secs: 1_788_192_100,
      last_reported_at_unix_secs: 1_788_192_200,
      calibrated_at_unix_secs: null,
      last_gap_at_unix_secs: 1_788_192_150,
      last_gap_reason: 'machine-reboot',
      has_gap: true,
    },
  ],
};

describe('machine list traffic accounting label', () => {
  it.each([
    ['inbound_only', '单向入站', '单·12.00 GiB'],
    ['outbound_only', '单向出站', '单·12.00 GiB'],
    ['bidirectional_max', '双向取大', '单·12.00 GiB'],
    ['bidirectional_sum', '双向合计', '12.00 GiB'],
  ] as const)('shows the saved %s accounting mode beside its total', (accounting_mode, label, total) => {
    const { container } = render(
      <NicMonthTotal
        traffic={{ ...view.nodes[0], accounting_mode, total_bytes: '12884901888' }}
        pending={false}
        retired={false}
      />,
    );

    const summary = container.querySelector('.lst-sum');
    expect(summary?.textContent).toBe(`${total}本月`);
    expect(summary?.getAttribute('title')).toContain(`${label}\n`);
    expect(summary?.getAttribute('title')).toContain('（UTC）');
    expect(summary?.getAttribute('title')).toContain('造成采集缺口');
  });

  it('keeps an unreported total unknown and labels a calibrated zero as single-direction usage', () => {
    const traffic: NodeTrafficItem = {
      ...view.nodes[0],
      accounting_mode: 'outbound_only',
      total_bytes: '0',
      tracking_started_at_unix_secs: null,
      calibrated_at_unix_secs: null,
    };
    const { container, rerender } = render(<NicMonthTotal traffic={traffic} pending={false} retired={false} />);
    expect(container.querySelector('.lst-sum')?.textContent).toBe('—本月');

    rerender(
      <NicMonthTotal
        traffic={{ ...traffic, calibrated_at_unix_secs: view.nodes[0].period_start_unix_secs }}
        pending={false}
        retired={false}
      />,
    );
    expect(container.querySelector('.lst-sum')?.textContent).toBe('单·0 B本月');
  });
});

describe('node traffic accounting values', () => {
  const start = Date.parse('2026-10-14T10:00:00Z') / 1000;
  const usage = (buckets: UsageNodeSeries['buckets']): UsageNodeSeries => ({
    node_id: 'n1',
    month_user_uplink_bytes: 0,
    month_user_downlink_bytes: 0,
    month_relay_uplink_bytes: 0,
    month_relay_downlink_bytes: 0,
    month_has_gap: false,
    buckets,
  });
  const bucket = (end: number, user = 30, relay = 15): UsageNodeSeries['buckets'][number] => ({
    window_end: new Date(end * 1000).toISOString(),
    user_uplink_bytes: user,
    user_downlink_bytes: 0,
    relay_uplink_bytes: relay,
    relay_downlink_bytes: 0,
  });

  it('keeps the last reported rates and exact times while the next usage window is pending', () => {
    const result = usageRoleTimeline(usage([bucket(start + 5), bucket(start + 35, 60, 30)]), start, start + 90);

    expect(result.timesUnixSecs).toEqual([start + 5, start + 35]);
    expect(result.user).toEqual([8, 16]);
    expect(result.relay).toEqual([4, 8]);
  });

  it('breaks missing reports and preserves reported idle windows without filling the range', () => {
    const result = usageRoleTimeline(
      usage([bucket(start + 95, 0, 0), bucket(start + 5), bucket(start + 35)]),
      start,
      start + 150,
    );

    expect(result.timesUnixSecs).toEqual([start + 5, start + 35, start + 65, start + 95]);
    expect(result.user).toEqual([8, 8, null, 0]);
    expect(result.relay).toEqual([4, 4, null, 0]);
  });

  it('leaves absent, empty and out-of-range usage unmeasured', () => {
    const invalid = { ...bucket(start), window_end: 'invalid' };
    for (const source of [undefined, usage([]), usage([bucket(start - 1), bucket(start + 91), invalid])]) {
      const result = usageRoleTimeline(source, start, start + 90);
      expect(result.timesUnixSecs).toEqual([]);
      expect(result.user).toEqual([]);
      expect(result.relay).toEqual([]);
    }
  });

  it('uses real forwarding duration rather than the access 30-second cadence', () => {
    const source: UsageNodeSeries = {
      node_id: 'n1',
      month_user_uplink_bytes: 0,
      month_user_downlink_bytes: 0,
      month_relay_uplink_bytes: 0,
      month_relay_downlink_bytes: 0,
      month_has_gap: false,
      buckets: [],
      forwarding: [
        {
          window_start: '2026-10-14T10:00:00Z',
          window_end: '2026-10-14T10:00:15Z',
          uplink_bytes: 15,
          downlink_bytes: 30,
          has_gap: false,
        },
        {
          window_start: '2026-10-14T10:00:15Z',
          window_end: '2026-10-14T10:01:00Z',
          uplink_bytes: 45,
          downlink_bytes: 90,
          has_gap: false,
        },
        {
          window_start: '2026-10-14T10:01:00Z',
          window_end: '2026-10-14T10:01:15Z',
          uplink_bytes: 15,
          downlink_bytes: 30,
          has_gap: true,
        },
      ],
    };
    const start = Date.parse('2026-10-14T10:00:00Z') / 1000;
    const result = usageRoleTimeline(source, start, start + 90);
    expect(result.forwarding).toEqual([24, 24, null]);
    expect(result.forwardingTimes).toEqual([start + 15, start + 60, start + 75]);
    expect(result.forwardingStarts).toEqual([start, start + 15, start + 60]);
  });
  it('converts human IEC calibration input to exact byte strings', () => {
    expect(trafficCalibrationBytes('1.5', 'GiB')).toBe('1610612736');
    expect(trafficCalibrationBytes('2', 'TiB')).toBe('2199023255552');
    expect(trafficCalibrationBytes('0', 'GiB')).toBe('0');
  });

  it('rejects negative, over-precise and u64-overflow calibration input', () => {
    expect(trafficCalibrationBytes('-1', 'GiB')).toBeNull();
    expect(trafficCalibrationBytes('1.0000001', 'GiB')).toBeNull();
    expect(trafficCalibrationBytes('18446744073709551616', 'TiB')).toBeNull();
  });

  it('formats decimal-string totals without requiring a safe JS integer', () => {
    expect(exactBytes('1023')).toBe('1023 B');
    expect(exactBytes('1099511627776')).toBe('1.00 TiB');
    expect(exactBytes('not-a-counter')).toBe('—');
  });

  it('stays expanded and saves quota, yearly UTC reset policy, and calibration together', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => view,
    });
    vi.stubGlobal('fetch', fetchMock);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <TrafficAccountingCard node={{ node_id: 'hk', name: '香港' } as NodeAgentStateItem} canEdit />
      </QueryClientProvider>,
    );

    const title = await screen.findByText('流量统计');
    const panel = title.closest('.node-traffic-accounting');
    expect(panel?.tagName).toBe('DIV');
    expect(panel?.querySelector('summary')).toBeNull();
    expect(screen.queryByText('缺口')).toBeNull();
    expect(panel?.querySelector('.node-traffic-used b')?.textContent).toBe('1.50 TiB');
    // 没有额度时只有读数，不画额度条也不写百分比。
    expect(panel?.querySelector('.node-traffic-meter')).toBeNull();
    expect(panel?.querySelector('.node-traffic-used')?.textContent).not.toContain('%');
    expect(screen.queryByText('缺口 · 机器重启')).toBeNull();
    expect(screen.queryByText(/才开始记账/)).toBeNull();
    expect(screen.getByLabelText('接收与发送流量').textContent).toContain('1.00 TiB');
    expect(screen.queryByText(/Agent 每 10 秒原子落盘/)).toBeNull();
    expect(screen.getByText('eth0')).toBeTruthy();
    expect(screen.getByText('接收与发送相加。')).toBeTruthy();
    fireEvent.change(screen.getByLabelText('机器流量计量口径'), {
      target: { value: 'bidirectional_max' },
    });
    // 说明只写当前选中的口径。
    expect(screen.queryByText('接收与发送相加。')).toBeNull();
    expect(screen.getByText('整个账期累计后取较大的方向，不按天取大。')).toBeTruthy();
    const quota = screen.getByLabelText('机器流量额度');
    fireEvent.change(quota, { target: { value: '2.555' } });
    expect(quota.getAttribute('aria-invalid')).toBe('true');
    expect(screen.getByText(/流量额度需为大于 0 的数字、最多两位小数/)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '每年' }));
    fireEvent.change(screen.getByLabelText('重置月份'), { target: { value: '2' } });
    fireEvent.change(screen.getByLabelText('重置日'), { target: { value: '29' } });
    fireEvent.change(quota, { target: { value: '2.5' } });
    // 校准是一次动作，不是常驻字段：点「校准」才出现输入框。
    expect(screen.queryByLabelText('当前流量校准值')).toBeNull();
    expect(screen.getByText('本期未校准')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '校准' }));
    fireEvent.change(screen.getByLabelText('当前流量校准值'), { target: { value: '1.5' } });
    // 保存栏只有按钮，不逐条复述改动。
    expect(document.querySelector('.config-panel-savebar')?.textContent).toBe('保存还原');
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const [url, init] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(url).toBe('/node-traffic/nodes/hk');
    expect(JSON.parse(String(init.body))).toEqual({
      cycle_kind: 'yearly',
      accounting_mode: 'bidirectional_max',
      reset_month: 2,
      reset_day: 29,
      quota_bytes: '2748779069440',
      calibrated_total_bytes: '1610612736',
    });
  });

  const TIB = 1024 ** 4;
  const DAY = 86_400;

  function renderCard(patch: Partial<NodeTrafficItem>, canEdit = true) {
    const item = { ...view.nodes[0], ...patch };
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => ({ nodes: [item] }) }));
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData<NodeTrafficView>(['node-traffic'], { nodes: [item] });
    render(
      <QueryClientProvider client={client}>
        <TrafficAccountingCard node={{ node_id: 'hk', name: '香港' } as NodeAgentStateItem} canEdit={canEdit} />
      </QueryClientProvider>,
    );
    const panel = document.querySelector('.node-traffic-accounting');
    expect(panel).toBeTruthy();
    return panel!;
  }

  it('compares the used share of the quota with the elapsed share of the period', () => {
    const now = Math.floor(Date.now() / 1000);
    const end = now + 3.25 * DAY;
    const panel = renderCard({
      quota_bytes: String(2 * TIB),
      total_bytes: String(1.5 * TIB),
      period_start_unix_secs: now - 10 * DAY,
      period_end_unix_secs: end,
    });

    const used = panel.querySelector('.node-traffic-used')?.textContent ?? '';
    expect(used).toContain('1.50 TiB');
    expect(used).toContain('/ 2 TiB');
    expect(used).toContain('75%');
    expect(panel.querySelector('.node-traffic-used .over')).toBeNull();
    expect(panel.querySelector('.node-traffic-reset')?.textContent).toBe(
      `3 天后重置 · ${new Date(end * 1000).toISOString().slice(5, 10)} UTC`,
    );
    const pace = panel.querySelector<HTMLElement>('.node-traffic-pace');
    expect(pace).toBeTruthy();
    expect(Number.parseFloat(pace!.style.left)).toBeCloseTo((10 / 13.25) * 100, 0);
    expect(panel.querySelector('.cap-q')).toBeNull();
    expect(panel.querySelector('.cap-bar .u')).toBeTruthy();
  });

  it('marks traffic above the quota and dims the direction the accounting mode leaves out', () => {
    const panel = renderCard({
      accounting_mode: 'outbound_only',
      quota_bytes: String(TIB),
      rx_bytes: String(0.5 * TIB),
      tx_bytes: String(1.5 * TIB),
      total_bytes: String(1.5 * TIB),
    });

    expect(panel.querySelector('.node-traffic-used .over')?.textContent).toBe('150%');
    expect(panel.querySelector('.cap-bar .u.x')).toBeTruthy();
    expect(panel.querySelector('.cap-q')).toBeTruthy();
    expect(panel.querySelector('.node-traffic-pace')).toBeNull();
    expect(panel.querySelector('.node-traffic-directions .rx')?.classList.contains('off')).toBe(true);
    expect(panel.querySelector('.node-traffic-directions .tx')?.classList.contains('off')).toBe(false);
    expect(screen.getByText('只计发送。')).toBeTruthy();
  });

  it('shows the calibration state to read-only viewers without edit controls', () => {
    const panel = renderCard({ calibrated_at_unix_secs: Math.floor(Date.now() / 1000) - 3 * DAY }, false);

    expect(panel.textContent).toContain('已校准 · 3');
    expect(screen.queryByRole('button', { name: '校准' })).toBeNull();
    expect(screen.queryByLabelText('当前流量校准值')).toBeNull();
    expect((screen.getByLabelText('机器流量计量口径') as HTMLSelectElement).disabled).toBe(true);
    expect(panel.querySelector('.config-panel-savebar')).toBeNull();
  });
});
