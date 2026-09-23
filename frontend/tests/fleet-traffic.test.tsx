import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { NodeNicSample, NodeNicView } from '../src/api';
import { FleetTrafficMeter, summarizeFleetTraffic } from '../src/ui/fleet-traffic';

const sample = (end: number, rx: number, tx: number, hasGap = false): NodeNicSample => ({
  window_start_unix_secs: end - 30,
  window_end_unix_secs: end,
  has_gap: hasGap,
  nic_rx_bps: rx,
  nic_tx_bps: tx,
});

const nodes: NodeNicView[] = [
  {
    node_id: 'tokyo',
    series: [sample(1_940, 500_000_000, 400_000_000), sample(1_970, 1_000_000_000, 958_000_000)],
  },
  {
    node_id: 'hong-kong',
    series: [sample(1_945, 300_000_000, 200_000_000), sample(1_975, 200_000_000, 100_000_000)],
  },
  { node_id: 'offline', series: [sample(1_800, 8_000_000_000, 8_000_000_000)] },
  { node_id: 'gap', series: [sample(1_970, 9_000_000_000, 9_000_000_000, true)] },
];

const nodesAt = (now: number): NodeNicView[] => {
  const offset = now - 2_000;
  return nodes.map(node => ({
    ...node,
    series: node.series.map(point => ({
      ...point,
      window_start_unix_secs: point.window_start_unix_secs + offset,
      window_end_unix_secs: point.window_end_unix_secs + offset,
    })),
  }));
};

class Events {
  static current: Events;
  handlers = new Map<string, (event: { data: string }) => void>();
  onerror: (() => void) | null = null;
  onopen: (() => void) | null = null;
  constructor(readonly url: string) {
    Events.current = this;
  }
  addEventListener(name: string, callback: (event: { data: string }) => void) {
    this.handlers.set(name, callback);
  }
  close() {}
  send(name: string, value: unknown) {
    this.handlers.get(name)?.({ data: JSON.stringify(value) });
  }
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('fleet traffic meter', () => {
  it('sums only fresh latest readings and excludes gaps', () => {
    expect(summarizeFleetTraffic(nodes, 2_000)).toEqual({
      rxBps: 1_200_000_000,
      txBps: 1_058_000_000,
      peakBps: 1_200_000_000,
      latestWindowEnd: 1_975,
      sampledNodes: 2,
      totalNodes: 4,
    });
  });

  it('renders compact RX and TX readings on one unit rung', async () => {
    const now = Math.floor(Date.now() / 1_000);
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(Response.json({ nodes: nodesAt(now) })));
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={queryClient}>
        <FleetTrafficMeter />
      </QueryClientProvider>,
    );

    const meter = await screen.findByLabelText('整批机器实时流量');
    expect(meter.textContent).toContain('1.20Gbit/s');
    expect(meter.textContent).toContain('1.06Gbit/s');
    expect(meter.textContent).not.toContain('峰值');
    expect(meter.textContent).not.toContain('近 1 小时');
    expect(screen.getByLabelText('接收 1.20 Gbit/s')).toBeTruthy();
    expect(screen.getByLabelText('发送 1.06 Gbit/s')).toBeTruthy();
  });

  it('does not present an empty or stale fleet as zero traffic', async () => {
    const now = Math.floor(Date.now() / 1_000);
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(Response.json({ nodes: [{ node_id: 'offline', series: [sample(now - 200, 1, 1)] }] })),
    );
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={queryClient}>
        <FleetTrafficMeter />
      </QueryClientProvider>,
    );

    expect(await screen.findByText('尚无实时网卡读数')).toBeTruthy();
    expect(screen.queryByText('0.00 bit/s')).toBeNull();
  });

  it('uses the fleet SSE stream for current RX and TX on a one-second display cadence', async () => {
    vi.stubGlobal('EventSource', Events);
    const now = Date.now();
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(Response.json({ nodes: nodesAt(Math.floor(now / 1_000)) })));
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={queryClient}>
        <FleetTrafficMeter />
      </QueryClientProvider>,
    );

    await screen.findByLabelText('整批机器实时流量');
    expect(Events.current.url).toBe('/realtime/nodes/events');
    act(() => {
      Events.current.send('snapshot', {
        nodes: [
          { node_id: 'tokyo', connected: true, active: true, interval_secs: 1, samples: [] },
          { node_id: 'hong-kong', connected: true, active: true, interval_secs: 1, samples: [] },
          { node_id: 'offline', connected: false, active: false, interval_secs: 1, samples: [] },
          { node_id: 'gap', connected: true, active: true, interval_secs: 1, samples: [] },
        ],
      });
      Events.current.send('sample', {
        node_id: 'tokyo',
        received_at_unix_millis: now,
        sample: { rx_bytes_per_sec: 25_000_000, tx_bytes_per_sec: 10_000_000, has_gap: false },
      });
    });

    expect(screen.queryByLabelText('接收 200 Mbit/s')).toBeNull();
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 1_050));
    });
    expect(screen.getByLabelText('接收 200 Mbit/s')).toBeTruthy();
    expect(screen.getByLabelText('发送 80.0 Mbit/s')).toBeTruthy();
    expect(screen.queryByText(/峰值|近 1 小时/)).toBeNull();
  });
});
