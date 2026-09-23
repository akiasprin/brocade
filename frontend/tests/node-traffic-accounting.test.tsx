import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, NodeTrafficView } from '../src/api';

let exactBytes: typeof import('../src/panes/nodes').exactBytes;
let trafficCalibrationBytes: typeof import('../src/panes/nodes').trafficCalibrationBytes;
let TrafficAccountingCard: typeof import('../src/panes/nodes').TrafficAccountingCard;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
  ({ exactBytes, trafficCalibrationBytes, TrafficAccountingCard } = await import('../src/panes/nodes'));
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
      reset_month: null,
      reset_day: 1,
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

describe('node traffic accounting values', () => {
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

  it('saves yearly UTC reset policy and a total-only calibration immediately', async () => {
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
    const panel = title.closest('details');
    expect(panel?.open).toBe(false);
    expect(screen.getByText('缺口')).toBeTruthy();
    fireEvent.click(title.closest('summary')!);
    expect(panel?.open).toBe(true);
    expect(screen.getByText('本期总量')).toBeTruthy();
    expect(screen.getByText('缺口 · 机器重启')).toBeTruthy();
    expect(screen.getByLabelText('接收与发送流量').textContent).toContain('1.00 TiB');
    expect(screen.queryByText(/Agent 每 10 秒原子落盘/)).toBeNull();
    expect(screen.getByText('eth0')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '每年' }));
    fireEvent.change(screen.getByLabelText('重置月份'), { target: { value: '2' } });
    fireEvent.change(screen.getByLabelText('重置日'), { target: { value: '29' } });
    fireEvent.change(screen.getByLabelText('当前流量校准值'), { target: { value: '1.5' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const [url, init] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(url).toBe('/node-traffic/nodes/hk');
    expect(JSON.parse(String(init.body))).toEqual({
      cycle_kind: 'yearly',
      reset_month: 2,
      reset_day: 29,
      calibrated_total_bytes: '1610612736',
    });
  });
});
