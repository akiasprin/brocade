import { QueryClient } from '@tanstack/react-query';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { initialNodeDetailFromHash } from '../src/panes/preload';

let prefetchNodeDetailData: typeof import('../src/panes/nodes').prefetchNodeDetailData;

beforeAll(async () => {
  ({ prefetchNodeDetailData } = await import('../src/panes/nodes'));
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const json = (value: unknown) =>
  new Response(JSON.stringify(value), { status: 200, headers: { 'content-type': 'application/json' } });

const emptyLoadOverview = {
  node_id: 'n1',
  range_start_unix_secs: 0,
  range_end_unix_secs: 0,
  reported_at_unix_secs: null,
  clock_skew_secs: null,
  host: null,
  latest_sample: null,
  processes: [],
  series: {
    window_start_unix_secs: [],
    window_end_unix_secs: [],
    has_gap: [],
    cpu_user_pct: [],
    cpu_sys_pct: [],
    cpu_softirq_pct: [],
    cpu_peak_pct: [],
    cpu_steal_pct: [],
    load1: [],
    mem_available_bytes: [],
    swap_used_bytes: [],
    oom_kills: [],
    disk_free_bytes: [],
    disk_inode_free_pct: [],
    nic_rx_bps: [],
    nic_tx_bps: [],
    nic_rx_drop: [],
    nic_tx_drop: [],
    nic_err: [],
    conntrack_count: [],
    uptime_secs: [],
  },
};

function responseFor(path: string): Response {
  if (path.startsWith('/load/nodes/n1/overview?')) return json(emptyLoadOverview);
  if (path === '/usage/node-series?window_secs=3600&node_id=n1') {
    return json({ since: '', month_start: '', nodes: [] });
  }
  if (path === '/ping-probe/nodes/n1/series?window_secs=3600') return json({ node_id: 'n1', targets: [] });
  if (path === '/model/snapshot') return json({ snapshot: { nodes: [], apps: [] } });
  if (path === '/deployments?limit=50') return json({ deployments: [] });
  throw new Error(`unexpected request ${path}`);
}

describe('machine-detail preparation', () => {
  it('only recognizes exact detail routes and safely decodes the node id', () => {
    expect(initialNodeDetailFromHash('#/nodes/node/edge-jp-01')).toBe('edge-jp-01');
    expect(initialNodeDetailFromHash('#/nodes/node/hk%2Fedge')).toBe('hk/edge');
    expect(initialNodeDetailFromHash('#/nodes/node/edge-jp-01?range=6h')).toBe('edge-jp-01');
    expect(initialNodeDetailFromHash('#/nodes/node/edge-jp-01?from=1700000000&to=1700000600')).toBe('edge-jp-01');
    expect(initialNodeDetailFromHash('#/nodes')).toBeNull();
    expect(initialNodeDetailFromHash('#/nodes/node')).toBeNull();
    expect(initialNodeDetailFromHash('#/nodes/node/n1/extra')).toBeNull();
    expect(initialNodeDetailFromHash('#/nodes/node/%E0%A4%A')).toBeNull();
  });

  it('warms one bounded window and refreshes it after it becomes stale', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-09-20T12:00:00Z'));
    const request = vi.fn(async (input: RequestInfo | URL) => responseFor(String(input)));
    vi.stubGlobal('fetch', request);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await prefetchNodeDetailData(client, 'n1', true);

    expect(request.mock.calls.map(call => String(call[0]))).toEqual(
      expect.arrayContaining([
        expect.stringMatching(/^\/load\/nodes\/n1\/overview\?/),
        '/usage/node-series?window_secs=3600&node_id=n1',
        '/ping-probe/nodes/n1/series?window_secs=3600',
        '/model/snapshot',
        '/deployments?limit=50',
      ]),
    );
    expect(request).toHaveBeenCalledTimes(5);
    expect(client.getQueryData(['node-load-history', 'n1', 3600])).toBeTruthy();
    expect(client.getQueryData(['usage-node-series', 'n1', 3600])).toBeTruthy();
    expect(client.getQueryData(['node-ping-probe', 'n1', 3600])).toBeTruthy();

    await prefetchNodeDetailData(client, 'n1', true);
    expect(request).toHaveBeenCalledTimes(5);

    vi.advanceTimersByTime(10_001);
    await prefetchNodeDetailData(client, 'n1', true);
    // The three rolling observation reads refresh; the 30-second snapshot/deployment cache stays hot.
    expect(request).toHaveBeenCalledTimes(8);
  });
});
