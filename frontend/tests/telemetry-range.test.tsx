import { cleanup, fireEvent, render } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import {
  fetchLatestNodePingProbes,
  fetchNodeLoadOverview,
  fetchNodePingProbeRange,
  fetchUsageNodeSeries,
  fetchUsageNodeSeriesRange,
} from '../src/api';
import type { LoadRange } from '../src/panes/nodes';

let ObserveRangeControl: typeof import('../src/panes/nodes').ObserveRangeControl;
let ObserveLinkControl: typeof import('../src/panes/nodes').ObserveLinkControl;
let LOAD_RANGES: typeof import('../src/panes/nodes').LOAD_RANGES;
let DEFAULT_LOAD_RANGE: typeof import('../src/panes/nodes').DEFAULT_LOAD_RANGE;
let TcpProbeLatest: typeof import('../src/panes/nodes').TcpProbeLatest;
let pingRefreshMillis: typeof import('../src/panes/nodes').pingRefreshMillis;
let nodeListSnapshotFresh: typeof import('../src/panes/nodes').nodeListSnapshotFresh;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  ({
    ObserveRangeControl,
    ObserveLinkControl,
    LOAD_RANGES,
    DEFAULT_LOAD_RANGE,
    TcpProbeLatest,
    pingRefreshMillis,
    nodeListSnapshotFresh,
  } = await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('machine telemetry range', () => {
  it('defaults machine detail observations to the latest hour', () => {
    expect(DEFAULT_LOAD_RANGE).toMatchObject({ seconds: 60 * 60, label: '1h', menuLabel: '近 1 小时' });
  });

  it('slows long PING refreshes, never polls fixed history and distrusts an old machine cache', () => {
    expect(pingRefreshMillis(LOAD_RANGES[1])).toBe(10_000);
    expect(pingRefreshMillis(LOAD_RANGES[2])).toBe(30_000);
    expect(pingRefreshMillis(LOAD_RANGES[4])).toBe(60_000);
    expect(
      pingRefreshMillis({
        ...LOAD_RANGES[1],
        startUnixSecs: 1_700_000_000,
        endUnixSecs: 1_700_003_600,
      }),
    ).toBe(false);

    expect(nodeListSnapshotFresh(1_000_000, 1_059_999)).toBe(true);
    expect(nodeListSnapshotFresh(1_000_000, 1_060_001)).toBe(false);
  });

  it('keeps chart linking off by default and exposes it as a labelled slider', () => {
    const Harness = () => {
      const [linked, setLinked] = useState(false);
      return <ObserveLinkControl value={linked} onChange={setLinked} />;
    };
    const view = render(<Harness />);
    const control = view.getByRole('switch', { name: '同组图表联动' });

    expect(control.getAttribute('aria-checked')).toBe('false');
    expect(view.getByText('同组图表联动')).toBeTruthy();
    fireEvent.click(control);
    expect(control.getAttribute('aria-checked')).toBe('true');
  });

  it('uses the clock dropdown, offers every full range label and switches the selected item', () => {
    const Harness = () => {
      const [selected, setSelected] = useState<LoadRange>(LOAD_RANGES[0]);
      return <ObserveRangeControl value={selected} onChange={setSelected} />;
    };
    const view = render(<Harness />);

    const trigger = view.getByRole('button', { name: '观测时间范围：近 30 分钟' });
    expect(trigger.classList.contains('btn')).toBe(true);
    expect(trigger.getAttribute('aria-expanded')).toBe('false');

    fireEvent.click(trigger);
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    expect(view.getAllByRole('option').map(option => option.textContent)).toEqual([
      '近 30 分钟✓',
      '近 1 小时✓',
      '近 6 小时✓',
      '近 12 小时✓',
      '近 24 小时✓',
    ]);
    expect(view.getByRole('option', { name: '近 30 分钟' }).getAttribute('aria-selected')).toBe('true');

    fireEvent.click(view.getByRole('option', { name: '近 24 小时' }));
    expect(view.getByRole('button', { name: '观测时间范围：近 24 小时' }).getAttribute('aria-expanded')).toBe('false');
    expect(view.queryByRole('listbox')).toBeNull();
  });

  it('closes the range menu on outside interaction and Escape', () => {
    const Harness = () => {
      const [selected, setSelected] = useState<LoadRange>(LOAD_RANGES[0]);
      return <ObserveRangeControl value={selected} onChange={setSelected} />;
    };
    const view = render(<Harness />);
    const trigger = view.getByRole('button', { name: '观测时间范围：近 30 分钟' });

    fireEvent.click(trigger);
    fireEvent.pointerDown(document.body);
    expect(trigger.getAttribute('aria-expanded')).toBe('false');

    fireEvent.click(trigger);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(trigger);
  });

  it('applies one fixed date range instead of turning its duration into a recent window', () => {
    const Harness = () => {
      const [selected, setSelected] = useState<LoadRange>(LOAD_RANGES[0]);
      return (
        <>
          <ObserveRangeControl value={selected} onChange={setSelected} />
          <output>{`${selected.startUnixSecs ?? ''}|${selected.endUnixSecs ?? ''}`}</output>
        </>
      );
    };
    const view = render(<Harness />);
    fireEvent.click(view.getByRole('button', { name: '观测时间范围：近 30 分钟' }));
    fireEvent.change(view.getByLabelText('观测开始时间'), { target: { value: '2026-09-06T10:00' } });
    fireEvent.change(view.getByLabelText('观测结束时间'), { target: { value: '2026-09-06T12:30' } });
    fireEvent.click(view.getByRole('button', { name: '应用时间范围' }));

    const start = Math.floor(new Date('2026-09-06T10:00').getTime() / 1000);
    const end = Math.floor(new Date('2026-09-06T12:30').getTime() / 1000);
    expect(view.getByText(`${start}|${end}`)).toBeTruthy();
  });

  it('narrows a long Xray series request to the current machine', async () => {
    const fetchMock = vi.fn(
      async (_input: RequestInfo | URL) => new Response(JSON.stringify({ since: '', month_start: '', nodes: [] })),
    );
    vi.stubGlobal('fetch', fetchMock);

    await fetchUsageNodeSeries(86_400, 'akile-ogvtw-hinet');

    expect(String(fetchMock.mock.calls[0][0])).toBe('/usage/node-series?window_secs=86400&node_id=akile-ogvtw-hinet');
  });

  it('sends identical absolute boundaries to Xray and PING history endpoints', async () => {
    const fetchMock = vi.fn(
      async (_input: RequestInfo | URL) => new Response(JSON.stringify({ node_id: 'n1', targets: [] })),
    );
    vi.stubGlobal('fetch', fetchMock);

    await fetchUsageNodeSeriesRange(1_700_000_000, 1_700_003_600, 'n1');
    await fetchNodePingProbeRange('n1', 1_700_000_000, 1_700_003_600);

    expect(String(fetchMock.mock.calls[0][0])).toBe(
      '/usage/node-series?start_unix_secs=1700000000&end_unix_secs=1700003600&node_id=n1',
    );
    expect(String(fetchMock.mock.calls[1][0])).toBe(
      '/ping-probe/nodes/n1/series?start_unix_secs=1700000000&end_unix_secs=1700003600',
    );
  });

  it('expands exact columnar PING points without downsampling or filling loss', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        Response.json({
          node_id: 'n1',
          targets: [
            {
              name: '广东电信',
              address: 'icmp://example.test',
              probed_at_unix_secs: [101, 111, 121],
              attempted: [true, true, false],
              latency_us: [12_300, null, null],
            },
          ],
        }),
      ),
    );

    const view = await fetchNodePingProbeRange('n1', 100, 130);

    expect(view.targets[0].samples).toEqual([
      { probed_at_unix_secs: 101, attempted: true, latency_us: 12_300 },
      { probed_at_unix_secs: 111, attempted: true, latency_us: null },
      { probed_at_unix_secs: 121, attempted: false, latency_us: null },
    ]);
  });

  it('expands every overview point for ECharts without downsampling', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        Response.json({
          node_id: 'n1',
          range_start_unix_secs: 100,
          range_end_unix_secs: 130,
          reported_at_unix_secs: 130,
          clock_skew_secs: 0,
          host: null,
          latest_sample: null,
          processes: [],
          series: {
            window_start_unix_secs: [100, 110, 120],
            window_end_unix_secs: [110, 120, 130],
            has_gap: [false, true, false],
            cpu_user_pct: [1, 2, 3],
            cpu_sys_pct: [4, 5, 6],
            cpu_softirq_pct: [7, 8, 9],
            cpu_peak_pct: [10, 11, 12],
            cpu_steal_pct: [0, 0, 0],
            load1: [0.1, 0.2, 0.3],
            mem_available_bytes: [1000, 900, 800],
            swap_used_bytes: [0, 1, 2],
            oom_kills: [0, 0, 1],
            disk_free_bytes: [3000, 2900, 2800],
            disk_inode_free_pct: [99, 98, 97],
            nic_rx_bps: [101, 102, 103],
            nic_tx_bps: [201, 202, 203],
            nic_rx_drop: [0, 0, 1],
            nic_tx_drop: [0, 1, 1],
            nic_err: [0, 0, 0],
            conntrack_count: [10, null, 12],
            uptime_secs: [100, 110, 120],
          },
        }),
      ),
    );

    const view = await fetchNodeLoadOverview('n1', 100, 130);

    expect(view.series).toHaveLength(3);
    expect(view.series.map(sample => sample.cpu_user_pct)).toEqual([1, 2, 3]);
    expect(view.series.map(sample => sample.window_end_unix_secs)).toEqual([110, 120, 130]);
    expect(view.series[1]).toMatchObject({ has_gap: true, conntrack_count: null });
    expect(view.series.every(sample => sample.cpu_detail === null && sample.network_detail === null)).toBe(true);
  });

  it('uses the bounded latest-observation endpoint for machine cards', async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL) => Response.json({ interval_secs: 60, nodes: [] }));
    vi.stubGlobal('fetch', fetchMock);

    await fetchLatestNodePingProbes();

    expect(String(fetchMock.mock.calls[0][0])).toBe('/ping-probe/nodes/latest');
  });

  it('renders the latest TCP attempt without deriving a percentile', () => {
    const now = Math.floor(Date.now() / 1000);
    const view = render(
      <TcpProbeLatest
        intervalSecs={60}
        view={{
          node_id: 'n1',
          targets: [
            {
              name: 'TCP',
              address: 'tcp://192.0.2.1:443',
              latest: { probed_at_unix_secs: now, attempted: true, latency_us: 37_250 },
            },
          ],
        }}
      />,
    );

    expect(view.container.textContent).toBe('37ms');
    expect(view.container.textContent).not.toContain('P95');
    expect(view.container.querySelector('.nc-tcp-latest')?.getAttribute('title')).toContain('最新样本');
  });
});
