import { StrictMode } from 'react';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  HostFacts,
  LoadSample,
  NodeLoadView,
  NodePingProbeView,
  PingProbeFamilySeries,
  PingProbePoint,
} from '../src/api';
import type { LoadRange } from '../src/panes/nodes';
import {
  observeAreaFill,
  observeBpsReading,
  observeBpsUnit,
  observeBytesUnit,
  observeCountUnit,
  observeTimeTick,
  observeValueAxis,
} from '../src/ui/observe-chart';

const chartMock = vi.hoisted(() => ({
  setOption: vi.fn(),
  showLoading: vi.fn(),
  hideLoading: vi.fn(),
  connect: vi.fn(),
}));

vi.mock('echarts/core', () => ({
  use: vi.fn(),
  connect: chartMock.connect,
  init: vi.fn(() => ({
    setOption: chartMock.setOption,
    showLoading: chartMock.showLoading,
    hideLoading: chartMock.hideLoading,
    resize: vi.fn(),
    dispose: vi.fn(),
    group: '',
  })),
}));
vi.mock('echarts/charts', () => ({ CustomChart: {}, LineChart: {} }));
vi.mock('echarts/components', () => ({ GridComponent: {}, MarkLineComponent: {}, TooltipComponent: {} }));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

let LoadCard: typeof import('../src/panes/telemetry').LoadCard;
let ThroughputChart: typeof import('../src/panes/telemetry').ThroughputChart;
let downsampleKpiSeries: typeof import('../src/panes/telemetry').downsampleKpiSeries;
let loadFindings: typeof import('../src/panes/telemetry').loadFindings;
let HostCard: typeof import('../src/panes/nodes').HostCard;
let NicWave: typeof import('../src/panes/nodes').NicWave;
let ThroughputPanel: typeof import('../src/panes/nodes').ThroughputPanel;
let PingProbePanel: typeof import('../src/panes/nodes').PingProbePanel;
let PingLatencyChart: typeof import('../src/panes/node-observation-charts').PingLatencyChart;
let ObservationChartLoading: typeof import('../src/panes/node-observation-charts').ObservationChartLoading;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
  ({ LoadCard, ThroughputChart, downsampleKpiSeries, loadFindings } = await import('../src/panes/telemetry'));
  ({ HostCard, NicWave, ThroughputPanel, PingProbePanel } = await import('../src/panes/nodes'));
  ({ PingLatencyChart, ObservationChartLoading } = await import('../src/panes/node-observation-charts'));
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

beforeEach(() => {
  chartMock.setOption.mockClear();
  chartMock.showLoading.mockClear();
  chartMock.hideLoading.mockClear();
  chartMock.connect.mockClear();
});

it('uses the deep-metric ECharts loading treatment for regular observation charts', () => {
  render(<ObservationChartLoading />);

  expect(chartMock.showLoading).toHaveBeenCalledWith(
    'default',
    expect.objectContaining({
      text: '加载中…',
      maskColor: 'transparent',
      spinnerRadius: 8,
      lineWidth: 2,
    }),
  );
});

const host: HostFacts = {
  kernel: '6.8.0',
  cpu_model: 'Neoverse-N1',
  cores: 2,
  cpu_freq_max_mhz: 3000,
  cpu_governor: 'schedutil',
  cc_algo: 'bbr',
  available_cc: ['bbr'],
  default_qdisc: 'fq',
  nic_qdisc: 'fq',
  nic: 'eth0',
  nic_mtu: 1500,
  mem_total_bytes: 8 * 1024 ** 3,
  disk_total_bytes: 20 * 1024 ** 3,
  disk_mount: '/',
  disk_filesystem: 'ext4',
  disk_device: '/dev/vda1',
  disk_read_only: false,
  conntrack_max: 262144,
  ephemeral_port_low: 32768,
  ephemeral_port_high: 60999,
  ephemeral_port_capacity: 28232,
  sysctl_managed: true,
  arch: 'aarch64',
  os_pretty: 'Linux',
  virt: 'KVM',
  rmem_max: 1,
  wmem_max: 1,
  somaxconn: 1,
};

function sample(deep = true): LoadSample {
  return {
    window_start_unix_secs: 100,
    window_end_unix_secs: 130,
    has_gap: false,
    cpu_user_pct: 8,
    cpu_sys_pct: 4,
    cpu_softirq_pct: 6,
    cpu_peak_pct: 24,
    cpu_steal_pct: 0.2,
    load1: 0.42,
    cpu_detail: deep
      ? {
          iowait_pct: 0.3,
          load5: 0.38,
          load15: 0.31,
          pressure_some_pct: 0.08,
          io_pressure_some_pct: 0,
          io_pressure_full_pct: 0,
          procs_running: 1,
          procs_total: 100,
          context_switches_per_sec: 4820,
          net_rx_softirqs_per_sec: 18240,
          net_tx_softirqs_per_sec: 2410,
          throttled_usec: 0,
          frequency_mhz: 2800,
          cores: [
            { cpu: 0, user_pct: 12, system_pct: 7, softirq_pct: 13, iowait_pct: 0.2, steal_pct: 0.1 },
            { cpu: 1, user_pct: 8, system_pct: 4, softirq_pct: 5, iowait_pct: 0.4, steal_pct: 0.3 },
          ],
        }
      : null,
    mem_available_bytes: 3 * 1024 ** 3,
    swap_used_bytes: 128 * 1024 ** 2,
    memory_detail: deep
      ? {
          available_min_bytes: 2.8 * 1024 ** 3,
          free_bytes: 2 * 1024 ** 3,
          anon_bytes: 2.4 * 1024 ** 3,
          file_cache_bytes: 2.5 * 1024 ** 3,
          shmem_bytes: 128 * 1024 ** 2,
          kernel_other_bytes: 0.975 * 1024 ** 3,
          buffers_bytes: 32 * 1024 ** 2,
          kernel_reclaimable_bytes: 384 * 1024 ** 2,
          slab_unreclaimable_bytes: 118 * 1024 ** 2,
          unevictable_bytes: 32 * 1024 ** 2,
          mlocked_bytes: 20 * 1024 ** 2,
          dirty_bytes: 12 * 1024 ** 2,
          writeback_bytes: 0,
          swap_total_bytes: 2 * 1024 ** 3,
          swap_cached_bytes: 0,
          zswap_bytes: null,
          zswapped_bytes: null,
          gup_pinned_bytes: 12 * 1024 ** 2,
          swap_in_bytes: 0,
          swap_out_bytes: 0,
          pressure_some_pct: 0,
          pressure_full_pct: 0,
          major_faults: 0,
          direct_reclaim_pages: 0,
        }
      : null,
    oom_kills: 0,
    disk_free_bytes: 15 * 1024 ** 3,
    disk_inode_free_pct: 90,
    disk_detail: deep
      ? {
          total_bytes: 20 * 1024 ** 3,
          inode_total: 1_000_000,
          inode_free: 900_000,
          read_bps: 2 * 1024 ** 2,
          write_bps: 1024 ** 2,
          read_iops: 20.5,
          write_iops: 10.25,
          read_await_ms: 1.2,
          write_await_ms: 2.4,
          busy_pct: 12.5,
          queue_depth: 0.3,
          in_flight: 2,
          pressure_some_pct: 0.2,
          pressure_full_pct: 0,
        }
      : null,
    nic_rx_bps: 100,
    nic_tx_bps: 200,
    nic_rx_drop: 0,
    nic_tx_drop: 0,
    nic_err: 0,
    conntrack_count: 1200,
    network_detail: deep
      ? {
          tcp_curr_estab: 82,
          tcp_inuse: 95,
          tcp_time_wait: 31,
          tcp_orphan: 0,
          tcp_alloc: 142,
          tcp_mem_bytes: 512 * 1024,
          udp_inuse: 18,
          udp_mem_bytes: 64 * 1024,
          ephemeral_port_capacity: 28232,
          tcp_ephemeral_inuse_v4: 2200,
          tcp_ephemeral_inuse_v6: 800,
          tcp_ephemeral_time_wait_v4: 420,
          tcp_ephemeral_time_wait_v6: 80,
          tcp_ephemeral_top_target_v4: 1800,
          tcp_ephemeral_top_target_v6: 600,
          tcp_active_opens: 41,
          tcp_passive_opens: 36,
          tcp_attempt_fails: 2,
          tcp_estab_resets: 1,
          tcp_retrans_segs: 3,
          tcp_syn_retrans: 1,
          tcp_in_errors: 0,
          tcp_out_resets: 2,
          tcp_timeouts: 0,
          tcp_listen_overflows: 0,
          tcp_listen_drops: 0,
          udp_in_errors: 0,
          udp_no_ports: 1,
          udp_rcvbuf_errors: 0,
          udp_sndbuf_errors: 0,
        }
      : null,
    uptime_secs: 10000,
  };
}

function report(deep = true): NodeLoadView {
  const latest = sample(deep);
  return {
    node_id: 'n1',
    range_start_unix_secs: 0,
    range_end_unix_secs: 130,
    reported_at_unix_secs: 130,
    clock_skew_secs: 0,
    host,
    latest_sample: latest,
    series: [latest],
    processes: [],
  };
}

function reportWith(change: (sample: LoadSample) => void): NodeLoadView {
  const value = report();
  change(value.series[0]);
  return value;
}

const halfHour: LoadRange = { seconds: 30 * 60, label: '30m', menuLabel: '近 30 分钟', heading: '30 MINUTES' };

function renderThroughput(value: NodeLoadView, linked = false, range: LoadRange = halfHour) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData(['node-load-history', value.node_id, range.seconds], value);
  client.setQueryData(['usage-node-series', value.node_id, range.seconds], {
    since: '',
    month_start: '',
    nodes: [
      {
        node_id: value.node_id,
        buckets: [],
        month_user_uplink_bytes: 0,
        month_user_downlink_bytes: 0,
        month_relay_uplink_bytes: 0,
        month_relay_downlink_bytes: 0,
        month_has_gap: false,
      },
    ],
  });
  return render(
    <QueryClientProvider client={client}>
      <ThroughputPanel nodeId={value.node_id} range={range} linked={linked} />
    </QueryClientProvider>,
  );
}

describe('deep host telemetry', () => {
  it('only calls out CPU steal at eighty percent or above', () => {
    const below = reportWith(value => {
      value.cpu_steal_pct = 79.9;
    });
    const atThreshold = reportWith(value => {
      value.cpu_steal_pct = 80;
    });

    expect(loadFindings(below).some(finding => finding.chip.startsWith('宿主争抢'))).toBe(false);
    expect(loadFindings(atThreshold).map(finding => finding.chip)).toContain('宿主争抢 80.0%');
  });

  it('downsamples only KPI spark data while retaining endpoints, extrema and a gap marker', () => {
    const series = Array.from({ length: 1_000 }, (_, index) => {
      const point = sample(false);
      point.window_start_unix_secs = index * 30;
      point.window_end_unix_secs = (index + 1) * 30;
      point.cpu_user_pct = index === 417 ? 0 : index === 418 ? 100 : 20 + (index % 5);
      point.has_gap = index === 700;
      return point;
    });

    const sampled = downsampleKpiSeries(series, point => point.cpu_user_pct);

    expect(sampled.length).toBeLessThanOrEqual(160);
    expect(sampled[0]).toBe(series[0]);
    expect(sampled.at(-1)).toBe(series.at(-1));
    expect(sampled).toContain(series[417]);
    expect(sampled).toContain(series[418]);
    expect(sampled).toContain(series[700]);
    expect(series).toHaveLength(1_000);
  });

  it('keeps long host facts in wide cells with their complete values available', () => {
    const cpuModel = 'Intel(R) Xeon(R) Gold 6138 CPU @ 2.00GHz';
    const value: NodeLoadView = {
      ...report(),
      host: {
        ...host,
        cpu_model: cpuModel,
        cores: 1,
        os_pretty: 'Debian GNU/Linux 13 (trixie)',
        virt: 'KVM',
        kernel: '6.12.0-amd64',
        arch: 'x86_64',
        rmem_max: 128 * 1024 ** 2,
        wmem_max: 128 * 1024 ** 2,
      },
    };

    const view = render(<HostCard load={value} />);
    const cell = (label: string) => view.getByText(label).closest('.nd-rt-c');

    expect(cell('CPU')?.classList.contains('w3')).toBe(true);
    expect(cell('发行版')?.classList.contains('w3')).toBe(true);
    expect(cell('内核')?.classList.contains('w3')).toBe(true);
    expect(cell('收发缓冲')?.classList.contains('w2')).toBe(true);
    expect(view.getByTitle(`${cpuModel} · 1 核`)).toBeTruthy();
    expect(view.getByTitle('Debian GNU/Linux 13 (trixie) · KVM')).toBeTruthy();
    expect(view.getByTitle('6.12.0-amd64 · x86_64')).toBeTruthy();
    expect(view.getByTitle('128.0 MiB / 128.0 MiB')).toBeTruthy();
  });

  it('fills a node card from its first through last drawable window while retaining internal gaps', () => {
    const value = report();
    value.series = Array.from({ length: 7 }, (_, index) => {
      const next = sample();
      next.window_start_unix_secs = 100 + index * 30;
      next.window_end_unix_secs = 130 + index * 30;
      next.has_gap = index === 0 || index === 3 || index === 6;
      next.nic_rx_bps = 100 + index * 10;
      return next;
    });

    const view = render(<NicWave load={value} />);
    const lines = [...view.container.querySelectorAll('.node-nic-plot path.line')];
    expect(lines).toHaveLength(2);
    expect(lines[0].getAttribute('d')).toMatch(/^M0\.0,/);
    expect(lines[1].getAttribute('d')).toMatch(/100\.0,[0-9.]+$/);
  });

  it('uses seven time marks and advances the value ceiling by one standard tick', () => {
    const timeMarks = Array.from({ length: 60 }, (_, index) => index).filter(index => observeTimeTick(index, 60));
    expect(timeMarks).toEqual([0, 10, 20, 30, 39, 49, 59]);

    expect(observeValueAxis(873_410_000)).toEqual({ interval: 200_000_000, max: 1_000_000_000 });
    expect(observeValueAxis(500_000_000)).toEqual({ interval: 100_000_000, max: 600_000_000 });
    expect(observeValueAxis(100)).toEqual({ interval: 25, max: 125 });
  });

  it('keeps byte and compact readings on one card-wide unit while leaving ticks bare', () => {
    const byteUnit = observeBytesUnit(observeValueAxis(8 * 1024 ** 3));
    expect(byteUnit.name).toBe('GiB');
    expect(byteUnit.text(2 * 1024 ** 3)).toBe('2');
    expect(byteUnit.read(128 * 1024 ** 2)).toBe('0.125 GiB');

    const countUnit = observeCountUnit(observeValueAxis(262_144));
    expect(countUnit.name).toBe('k');
    expect(countUnit.text(50_000)).toBe('50');
    expect(countUnit.read(82)).toBe('0.0820 k');
  });

  it('spells network bit-rate units explicitly', () => {
    const unit = observeBpsUnit(observeValueAxis(82_000_000));
    expect(unit.name).toBe('Mbit/s');
    expect(unit.read(82_000_000)).toBe('82.0 Mbit/s');
    expect(observeBpsReading(1_000)).toBe('1.00 Kbit/s');
    expect(observeBpsReading(1_000_000_000)).toBe('1.00 Gbit/s');
  });

  it('fills unstacked traces with a tint of the series color, keeping the hue', () => {
    const stops = (fill: ReturnType<typeof observeAreaFill>) =>
      (fill as Exclude<typeof fill, string>).colorStops.map(stop => stop.color);

    // 12% of Rosé Pine pine over white paper: light enough that the split lines still read through.
    expect(stops(observeAreaFill('#286983', 'light'))).toEqual(['rgba(229,237,240,0.62)', 'rgba(229,237,240,0.341)']);
    expect(stops(observeAreaFill('#3e8fb0', 'dark'))).toEqual(['rgba(35,45,52,0.55)', 'rgba(35,45,52,0.3025)']);

    // Each trace keeps its own hue — the fill is what tells them apart under their own lines.
    expect(stops(observeAreaFill('#b66fa2', 'light'))[0]).toBe('rgba(246,238,244,0.62)');
  });

  it('thins the fill toward the zero line, where every unstacked trace piles up', () => {
    const gradient = observeAreaFill('#286983', 'light', { count: 3 }) as Exclude<
      ReturnType<typeof observeAreaFill>,
      string
    >;
    // Vertical, over the filled shape: offset 0 is the series peak, offset 1 the shared zero line.
    expect(gradient).toMatchObject({ type: 'linear', x: 0, y: 0, x2: 0, y2: 1 });

    const alphaAt = (offset: number) =>
      Number(gradient.colorStops.find(stop => stop.offset === offset)!.color.match(/,([\d.]+)\)$/)![1]);
    // The baseline is lighter than the band under the line, but it is still a fill — not nothing.
    expect(alphaAt(1)).toBeCloseTo(alphaAt(0) * 0.55, 4);
    expect(alphaAt(1)).toBeGreaterThan(0);

    // Both stops carry the same tinted color; only the alpha moves.
    const rgbOf = (color: string) => color.slice(0, color.lastIndexOf(','));
    expect(rgbOf(gradient.colorStops[1].color)).toBe(rgbOf(gradient.colorStops[0].color));
  });

  it('spreads the ink budget so a crowded chart does not repaint its floor', () => {
    const topAlpha = (count: number) => {
      const fill = observeAreaFill('#286983', 'light', { count }) as Exclude<
        ReturnType<typeof observeAreaFill>,
        string
      >;
      return Number(fill.colorStops[0].color.match(/,([\d.]+)\)$/)![1]);
    };
    expect(topAlpha(1)).toBeCloseTo(0.62, 3);
    expect(topAlpha(3)).toBeLessThan(topAlpha(1));
    expect(topAlpha(16)).toBeLessThan(topAlpha(3));
    expect(topAlpha(256)).toBe(0.1);
  });

  it('starts the time axis on the first sample, not on the minute below it', () => {
    // Samples land at :32 past the minute. Flooring the axis to 12:34:00 used to open 32 seconds
    // of blank between the y axis and the first point.
    const value = report();
    const firstEnd = 3632; // 01:00:32
    value.series = [0, 1, 2, 3].map(step => {
      const next = sample();
      next.window_start_unix_secs = firstEnd + step * 30 - 30;
      next.window_end_unix_secs = firstEnd + step * 30;
      return next;
    });
    const view = render(<LoadCard report={value} />);
    fireEvent.click(view.getByRole('button', { name: /CPU/ }));

    const chart = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === '用户态'),
    )?.[0];
    expect(chart.xAxis[0].min).toBe(firstEnd * 1000);
    expect(chart.xAxis[0].min % 60_000).not.toBe(0);
    expect(chart.xAxis[0].max).toBe((firstEnd + 90) * 1000);
    // The axis still starts at the first sample, but its ticks sit on whole wall-clock steps (30 s
    // here) instead of being counted from 01:00:32; a sub-minute step labels the seconds.
    expect(chart.xAxis[0].axisTick.customValues).toEqual([3_660_000, 3_690_000, 3_720_000]);
    expect(chart.xAxis[0].axisLabel.customValues).toEqual([3_660_000, 3_690_000, 3_720_000]);
    expect(chart.xAxis[0].axisLabel.formatter(3_690_000)).toMatch(/^\d{2}:\d{2}:30$/);
    expect(chart.xAxis[0].minorTick).toBeUndefined();
    expect(chart.xAxis[1]).toMatchObject({ min: firstEnd * 1000, max: (firstEnd + 90) * 1000, silent: true });
    expect(chart.xAxis[1].axisPointer).toEqual({ show: false, triggerTooltip: false });
  });

  it('keeps stacked bands flat and at full hue, since they never overlap', () => {
    expect(observeAreaFill('#286983', 'light', { stacked: true })).toBe('rgba(40,105,131,0.28)');
    expect(observeAreaFill('#3e8fb0', 'dark', { stacked: true })).toBe('rgba(62,143,176,0.34)');
  });

  it('shows the last known state when the selected interval has no samples', () => {
    const value = report();
    value.series = [];
    const view = render(<LoadCard report={value} />);

    expect(view.queryByText('LOAD')).toBeNull();
    expect(view.getByText(/所选时间范围内没有负载读数/)).toBeTruthy();
    expect(view.getByRole('button', { name: /CPU/ }).textContent).toContain('18');
  });

  it('shows the empty state only when the machine has never produced a sample', () => {
    const value = report();
    value.series = [];
    value.latest_sample = null;
    const view = render(<LoadCard report={value} />);

    expect(view.getByText('还没有负载读数。')).toBeTruthy();
  });

  it('mounts every deep EChart immediately and requests ten metrics per parallel batch', async () => {
    const value = report();
    value.series = value.series.map(entry => ({
      ...entry,
      cpu_detail: null,
      memory_detail: null,
      disk_detail: null,
      network_detail: null,
    }));
    let resolveFetch!: (response: Response) => void;
    const pendingResponse = new Promise<Response>(resolve => {
      resolveFetch = resolve;
    });
    const requestedBatches: string[][] = [];
    const metricResponse = (metrics: string[]) =>
      Response.json({
        node_id: 'n1',
        range_start_unix_secs: 0,
        range_end_unix_secs: 130,
        window_end_unix_secs: [130],
        has_gap: [false],
        metrics: Object.fromEntries(
          metrics.map(metric => [
            metric === 'cpu.cores.busy_pct' ? 'cpu.core.0.busy_pct' : metric,
            [metric.includes('iowait') ? 0.3 : 0],
          ]),
        ),
      });
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(input => {
      const url = String(input);
      const metrics = new URL(url, 'https://console.test').searchParams.get('metrics')?.split(',') ?? [];
      requestedBatches.push(metrics);
      return requestedBatches.length === 1 ? pendingResponse : Promise.resolve(metricResponse(metrics));
    });
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <LoadCard report={value} metricRangeKey="1h" />
      </QueryClientProvider>,
    );

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));

    expect(view.getByText('CPU 时间占比')).toBeTruthy();
    expect(view.getByText('CPU 与 I/O 压力')).toBeTruthy();
    expect(view.getByText('负载与运行队列')).toBeTruthy();
    expect(view.getByText('调度与网络事件速率')).toBeTruthy();
    expect(view.getByText('Cgroup 限流时间')).toBeTruthy();
    expect(view.getByText('逐核繁忙度 · 2 核')).toBeTruthy();
    expect(view.container.querySelectorAll('.history-chart-card[aria-busy="true"]')).toHaveLength(6);
    expect(chartMock.showLoading).toHaveBeenCalledTimes(6);
    expect(chartMock.setOption).not.toHaveBeenCalled();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(String(fetchMock.mock.calls[0][0])).toContain('/load/nodes/n1/metrics?');
    expect(requestedBatches.every(metrics => metrics.length <= 10)).toBe(true);
    expect(requestedBatches[0]).toContain('cpu.load5');
    expect(requestedBatches[0]).toHaveLength(10);
    expect(requestedBatches[1]).toContain('cpu.throttled_usec');
    expect(requestedBatches[1]).toContain('cpu.cores.busy_pct');

    await waitFor(() =>
      expect(
        chartMock.setOption.mock.calls.find(([option]) =>
          option.series?.some((line: { name: string }) => line.name === '每窗口 throttled'),
        ),
      ).toBeTruthy(),
    );
    expect(view.container.querySelectorAll('.history-chart-card[aria-busy="true"]')).toHaveLength(4);

    resolveFetch(metricResponse(requestedBatches[0]));

    await waitFor(() =>
      expect(
        chartMock.setOption.mock.calls.find(([option]) =>
          option.series?.some((line: { name: string }) => line.name === '用户态'),
        ),
      ).toBeTruthy(),
    );
    expect(view.container.querySelectorAll('.history-chart-card[aria-busy="true"]')).toHaveLength(0);
    expect(chartMock.hideLoading).toHaveBeenCalled();
    const cpuChart = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === '用户态'),
    )?.[0];
    expect(cpuChart.series[0].data).toHaveLength(1);
    fetchMock.mockRestore();
  });

  it('starts collapsed and switches between complete CPU, memory, disk and network histories', () => {
    const view = render(<LoadCard report={report()} />);
    expect(view.queryByRole('region', { name: 'CPU 30 MINUTES 数值' })).toBeNull();
    expect(view.queryByRole('region', { name: '内存 30 MINUTES 数值' })).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    const detailMotion = view.container.querySelector('.kpi-detail-motion');
    expect(detailMotion?.getAttribute('data-motion-state')).toBe('entering');
    expect(detailMotion?.getAttribute('aria-hidden')).toBeNull();
    expect(view.getByRole('region', { name: 'CPU 30 MINUTES 数值' })).toBeTruthy();
    expect(view.queryByText('CPU · 30 MINUTES')).toBeNull();
    expect(view.queryByText(/30 秒窗口 · 缺口断线/)).toBeNull();
    expect(view.getByText('CPU 时间占比')).toBeTruthy();
    const cpuLegend = view.getByLabelText('CPU 时间占比 图例');
    expect(cpuLegend.tagName).toBe('FOOTER');
    expect(cpuLegend.previousElementSibling?.classList.contains('history-chart')).toBe(true);
    expect(view.getByText('CPU 与 I/O 压力')).toBeTruthy();
    expect(view.getByText('负载与运行队列')).toBeTruthy();
    expect(view.getByText('逐核繁忙度 · 2 核')).toBeTruthy();
    expect(view.queryByText('诊断指标')).toBeNull();
    expect(view.queryByText('CPU 频率')).toBeNull();
    expect(view.queryByRole('region', { name: '内存 30 MINUTES 数值' })).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /内存/ }));
    expect(view.getByRole('region', { name: '内存 30 MINUTES 数值' })).toBeTruthy();
    expect(view.queryByText('MEMORY · 30 MINUTES')).toBeNull();
    expect(view.getByText('容量构成')).toBeTruthy();
    expect(view.getByText('内核缓存与固定页')).toBeTruthy();
    expect(view.getByText('Swap、脏页与回写')).toBeTruthy();
    expect(view.getByText('缺页与直接回收')).toBeTruthy();
    expect(view.queryByRole('region', { name: 'CPU 30 MINUTES 数值' })).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /内存/ }));
    expect(view.queryByRole('region', { name: '内存 30 MINUTES 数值' })).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /磁盘/ }));
    expect(view.getByRole('region', { name: '磁盘 30 MINUTES 数值' })).toBeTruthy();
    expect(view.queryByText('DISK · 30 MINUTES')).toBeNull();
    expect(view.getByText('块设备吞吐')).toBeTruthy();
    expect(view.getByText('块设备 IOPS')).toBeTruthy();
    expect(view.getByText('完成延迟')).toBeTruthy();
    expect(view.getByText('设备繁忙与 I/O 压力')).toBeTruthy();
    expect(view.getByText('队列')).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: /连接表/ }));
    expect(view.getByRole('region', { name: '网络 30 MINUTES 数值' })).toBeTruthy();
    expect(view.queryByText('NETWORK · 30 MINUTES')).toBeNull();
    expect(view.getByText('出站端口压力（估算）· 最繁忙目标')).toBeTruthy();
    expect(view.getByText('出站临时端口套接字')).toBeTruthy();
    expect(view.getByText('连接与套接字')).toBeTruthy();
    expect(view.getByText('TCP 连接生命周期')).toBeTruthy();
    expect(view.getByText('TCP 重传与异常')).toBeTruthy();
    expect(view.getByText('监听队列与 UDP 丢弃')).toBeTruthy();
    expect(view.getByText('套接字资源')).toBeTruthy();
    expect(view.queryByRole('region', { name: 'CPU 30 MINUTES 数值' })).toBeNull();
  });

  it('keeps the expanded KPI content mounted but inert while it collapses', async () => {
    const view = render(<LoadCard report={report()} />);
    const cpu = view.getByRole('button', { name: /CPU/ });

    fireEvent.click(cpu);
    await act(async () => Promise.resolve());
    fireEvent.click(cpu);

    const detailMotion = view.container.querySelector('.kpi-detail-motion');
    expect(detailMotion?.getAttribute('data-motion-state')).toBe('exiting');
    expect(detailMotion?.getAttribute('aria-hidden')).toBe('true');
    expect(detailMotion?.hasAttribute('inert')).toBe(true);
    expect(view.queryByRole('region', { name: 'CPU 30 MINUTES 数值' })).toBeNull();
  });

  it('moves history units into titles except for percentages, and keeps units on standalone readings', () => {
    const view = render(<LoadCard report={report()} />);

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    const percentHeader = view.getByText('CPU 时间占比').closest('header');
    expect(percentHeader?.querySelector('.chart-unit')).toBeNull();
    const percentChart = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === '用户态'),
    )?.[0];
    expect(percentChart.yAxis.axisLabel.formatter(25)).toBe('25.0%');

    fireEvent.click(view.getByRole('button', { name: /内存/ }));
    const memoryHeader = view.getByText('容量构成').closest('header');
    expect(memoryHeader?.querySelector('.chart-unit')?.textContent).toBe('(GiB)');
    expect(memoryHeader?.textContent).toContain('可用 3.00 GiB');
    expect(view.getByLabelText('容量构成 图例').textContent).toContain('匿名页 2.40 GiB');
    const memoryChart = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === '匿名页'),
    )?.[0];
    expect(memoryChart.yAxis.axisLabel.formatter(2 * 1024 ** 3)).toBe('2');
    const memoryTooltip = memoryChart.tooltip.formatter([
      { seriesName: '共享/tmpfs', color: '#111', value: [130_000, 128 * 1024 ** 2] },
    ]);
    expect(memoryTooltip).toContain('0.125 GiB');

    fireEvent.click(view.getByRole('button', { name: /磁盘/ }));
    expect(view.getByText('块设备吞吐').closest('header')?.querySelector('.chart-unit')?.textContent).toBe('(MiB/s)');
    expect(view.getByText('块设备 IOPS').closest('header')?.querySelector('.chart-unit')?.textContent).toBe('(/s)');
    expect(view.getByText('完成延迟').closest('header')?.querySelector('.chart-unit')?.textContent).toBe('(ms)');

    fireEvent.click(view.getByRole('button', { name: /连接表/ }));
    expect(view.getByText('连接与套接字').closest('header')?.querySelector('.chart-unit')?.textContent).toBe('(k)');
    expect(view.getByLabelText('连接与套接字 图例').textContent).toContain('TCP 已建立 0.0820 k');
  });

  it('keeps the network throughput legend and metadata in the dedicated throughput panel', async () => {
    const value = reportWith(sample => {
      sample.nic_rx_drop = 15;
    });
    const view = renderThroughput(value);
    const legend = view.getByLabelText('网卡流量图例');
    const header = view.getByText('网卡流量').parentElement;

    expect(legend.tagName).toBe('FOOTER');
    await waitFor(() => expect(view.container.querySelector('.ndtp-ec')).toBeTruthy());
    expect(header?.textContent).toContain('接收丢弃 15 · eth0 · MTU 1500');
    expect(header?.textContent).not.toContain('上报');
    expect(legend.textContent).not.toContain('接收丢弃');
    expect(legend.textContent).not.toContain('eth0');

    const chart = chartMock.setOption.mock.calls.find(
      ([option]) =>
        option.series?.length === 2 && option.series[0]?.name === '接收' && option.series[1]?.name === '发送',
    )?.[0];
    expect(chart.color).toEqual(['#3e8fb0', '#e99cd3']);
    expect(chart.grid).toMatchObject({ left: 10, containLabel: true });
    expect(chart.xAxis[0].splitLine.show).toBe(true);
    expect(chart.yAxis.splitLine.show).toBe(true);
    for (const line of chart.series) {
      expect(line.smooth).toBe(false);
      expect(line.areaStyle.color.type).toBe('linear');
      expect(line.areaStyle.color.colorStops[0].color).toMatch(/^rgba\(\d+,\d+,\d+,0\.3889\)$/);
      expect(line.areaStyle.color.colorStops[1].color).toMatch(/^rgba\(\d+,\d+,\d+,0\.2139\)$/);
      expect(line.areaStyle.opacity).toBe(1);
    }
    expect(chartMock.connect).not.toHaveBeenCalled();
  });

  it('connects throughput and expanded history charts only after linking is enabled', () => {
    const throughput = renderThroughput(report(), true);

    expect(chartMock.connect).toHaveBeenCalledWith('nd-tp-n1');
    throughput.unmount();
    const view = render(<LoadCard report={report()} linked />);
    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    expect(chartMock.connect).toHaveBeenCalledWith('nd-cpu-history-n1');
  });

  it('breaks NIC lines and fills across absent windows without dropping the recovery sample', async () => {
    const value = report();
    const before = sample();
    const after = { ...sample(), window_start_unix_secs: 1_000, window_end_unix_secs: 1_030, nic_rx_bps: 300 };
    value.series = [before, after];
    value.range_end_unix_secs = 1_030;
    value.latest_sample = after;
    renderThroughput(value);

    await waitFor(() => expect(chartMock.setOption).toHaveBeenCalled());
    const chart = chartMock.setOption.mock.calls.find(([option]) => option.series?.[0]?.name === '接收')?.[0];
    expect(chart.series[0].connectNulls).toBe(false);
    expect(chart.series[0].data).toEqual([
      [130_000, 100],
      [565_000, null],
      [1_030_000, 300],
    ]);
    expect(chart.series[1].data).toEqual([
      [130_000, 200],
      [565_000, null],
      [1_030_000, 200],
    ]);
  });

  it('uses the same explicit gap for every stacked deep-metric series', () => {
    const value = report();
    value.series = [sample(), { ...sample(), window_start_unix_secs: 1_000, window_end_unix_secs: 1_030 }];
    value.latest_sample = value.series[1];
    value.range_end_unix_secs = 1_030;
    const view = render(<LoadCard report={value} />);
    fireEvent.click(view.getByRole('button', { name: /CPU/ }));

    const chart = chartMock.setOption.mock.calls.find(([option]) => option.series?.[0]?.name === '用户态')?.[0];
    expect(chart).toBeTruthy();
    for (const series of chart.series) {
      expect(series.connectNulls).toBe(false);
      expect(series.data).toHaveLength(3);
      expect(series.data[1]).toEqual([565_000, null]);
      expect(series.data[2][0]).toBe(1_030_000);
      expect(series.data[2][1]).not.toBeNull();
    }
  });

  it('breaks Ping when all targets have no reports without labelling that interval packet loss', () => {
    render(
      <PingLatencyChart
        lines={[
          {
            name: '测试落点',
            color: 0,
            samples: [100, 110, 1_000, 1_010].map(probed_at_unix_secs => ({
              probed_at_unix_secs,
              attempted: true,
              latency_us: 12_000,
            })),
          },
        ]}
        family="ipv4"
        bounds={{ startUnixSecs: 0, endUnixSecs: 1_030 }}
      />,
    );
    const chart = chartMock.setOption.mock.calls.at(-1)?.[0];
    expect(chart.series).toHaveLength(1);
    expect(chart.series[0].connectNulls).toBe(false);
    expect(chart.series[0].data).toEqual([
      [100_000, 12],
      [110_000, 12],
      [555_000, null],
      [1_000_000, 12],
      [1_010_000, 12],
    ]);
    const missing = chart.tooltip.formatter([{ axisValue: 555_000 }]);
    expect(missing).toContain('—');
    expect(missing).not.toContain('无响应');
  });

  it('does not stretch a Ping loss lane across an unobserved interval', () => {
    render(
      <PingLatencyChart
        lines={[
          {
            name: '测试落点',
            color: 0,
            samples: [100, 110, 1_000, 1_010].map(probed_at_unix_secs => ({
              probed_at_unix_secs,
              attempted: true,
              latency_us: probed_at_unix_secs === 110 || probed_at_unix_secs === 1_000 ? null : 12_000,
            })),
          },
        ]}
        family="ipv4"
        bounds={{ startUnixSecs: 0, endUnixSecs: 1_030 }}
      />,
    );
    const chart = chartMock.setOption.mock.calls.at(-1)?.[0];
    expect(chart.series.at(-1).data).toEqual([
      [105_000, 115_000],
      [995_000, 1_005_000],
    ]);
  });

  it('sweeps Ping data in once and keeps later sample updates stable', () => {
    const lines = [
      { name: '测试落点', color: 0, samples: [{ probed_at_unix_secs: 100, attempted: true, latency_us: 12_000 }] },
    ];
    const view = render(
      <PingLatencyChart lines={lines} family="ipv4" bounds={{ startUnixSecs: 0, endUnixSecs: 130 }} />,
    );
    const initial = chartMock.setOption.mock.calls.at(-1)?.[0];

    expect(initial.animation).toBe(true);
    expect(initial.animationThreshold).toBe(10_000);
    expect(initial.animationDuration).toBe(360);

    view.rerender(
      <PingLatencyChart
        lines={[
          {
            ...lines[0],
            samples: [...lines[0].samples, { probed_at_unix_secs: 110, attempted: true, latency_us: 13_000 }],
          },
        ]}
        family="ipv4"
        bounds={{ startUnixSecs: 0, endUnixSecs: 130 }}
      />,
    );
    const updated = chartMock.setOption.mock.calls.at(-1)?.[0];
    expect(updated.animation).toBe(false);
    expect(updated.animationDuration).toBe(0);
  });

  it('does not interrupt the Ping entrance sweep when equivalent inputs get new references', () => {
    const lines = [
      {
        name: '测试落点',
        color: 0,
        samples: [
          { probed_at_unix_secs: 100, attempted: true, latency_us: 12_000 },
          { probed_at_unix_secs: 110, attempted: true, latency_us: null },
          { probed_at_unix_secs: 120, attempted: true, latency_us: 13_000 },
        ],
      },
    ];
    const bounds = { startUnixSecs: 0, endUnixSecs: 130 };
    const view = render(<PingLatencyChart lines={lines} family="ipv4" bounds={bounds} />);

    for (let i = 0; i < 3; i++) {
      view.rerender(<PingLatencyChart lines={structuredClone(lines)} family="ipv4" bounds={{ ...bounds }} />);
    }

    expect(chartMock.setOption).toHaveBeenCalledTimes(1);
    const initial = chartMock.setOption.mock.calls[0][0];
    expect(initial.animation).toBe(true);
    expect(initial.series[0].connectNulls).toBe(false);
    expect(initial.series[0].data).toEqual([
      [100_000, 12],
      [110_000, null],
      [120_000, 13],
    ]);

    view.rerender(<PingLatencyChart lines={lines} family="ipv4" bounds={{ startUnixSecs: 10, endUnixSecs: 140 }} />);
    expect(chartMock.setOption).toHaveBeenCalledTimes(2);
    expect(chartMock.setOption.mock.calls[1][0]).toMatchObject({
      animation: false,
      xAxis: [{ max: 140_000 }, { max: 140_000 }],
    });
  });

  it('paints every new Ping chart instance, including effect remounts and linking changes', () => {
    const lines = [
      { name: '测试落点', color: 0, samples: [{ probed_at_unix_secs: 100, attempted: true, latency_us: 12_000 }] },
    ];
    const bounds = { startUnixSecs: 0, endUnixSecs: 130 };
    const view = render(
      <StrictMode>
        <PingLatencyChart lines={lines} family="ipv4" bounds={bounds} />
      </StrictMode>,
    );
    expect(chartMock.setOption).toHaveBeenCalledTimes(2);

    view.rerender(
      <StrictMode>
        <PingLatencyChart lines={lines} family="ipv4" bounds={bounds} group="nd-ping-n1" />
      </StrictMode>,
    );
    expect(chartMock.setOption).toHaveBeenCalledTimes(3);
    expect(chartMock.connect).toHaveBeenCalledWith('nd-ping-n1');
  });

  it('marks loss periods in a lane below the axis and keeps the lane out of the tooltip', () => {
    const point = (at: number, latency_us: number | null, attempted = true): PingProbePoint => ({
      probed_at_unix_secs: at,
      attempted,
      latency_us,
    });
    render(
      <PingLatencyChart
        lines={[
          {
            name: '甲',
            color: 0,
            samples: [
              point(100, 10_000),
              point(110, null),
              point(120, null),
              point(130, 12_000),
              point(140, null, false),
            ],
          },
          { name: '乙', color: 1, samples: [point(100, 20_000), point(110, 21_000), point(130, null)] },
        ]}
        family="ipv6"
        bounds={{ startUnixSecs: 0, endUnixSecs: 150 }}
      />,
    );
    const option = chartMock.setOption.mock.calls.at(-1)?.[0];
    const lane = option.series.at(-1);
    // Consecutive losses form one period; a skipped probe draws nothing. Each loss covers half the
    // 10-second step on either side, so 甲 at 110–120 and 乙 at 130 touch and merge.
    expect(lane).toMatchObject({ type: 'custom', silent: true, clip: false, tooltip: { show: false } });
    expect(lane.data).toEqual([[105_000, 135_000]]);
    expect(option.series.filter((series: { type: string }) => series.type === 'line')).toHaveLength(2);
    const tooltip = option.tooltip.formatter([{ axisValue: 110_000 }]);
    expect(tooltip).toContain('· IPv6');
    expect(tooltip).toContain('无响应');
    expect(tooltip).toContain('21 ms');
  });

  it('anchors the live Ping axis to query refreshes, not parent renders or the wall clock', async () => {
    const now = vi.spyOn(Date, 'now').mockReturnValue(130_000);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const ping: NodePingProbeView = {
      node_id: 'n1',
      targets: [
        {
          name: '测试落点',
          kind: 'icmp',
          ipv4: {
            address: 'icmp://192.0.2.1',
            samples: [{ probed_at_unix_secs: 100, attempted: true, latency_us: 12_000 }],
          },
          ipv6: null,
        },
      ],
    };
    const queryKey = ['node-ping-probe', 'n1', halfHour.seconds];
    client.setQueryData(queryKey, ping);
    const observationModules = { LoadCard, ThroughputChart, PingLatencyChart, ObservationChartLoading };
    const panel = (range: LoadRange) => (
      <QueryClientProvider client={client}>
        <PingProbePanel nodeId="n1" range={range} linked={false} observationModules={observationModules} />
      </QueryClientProvider>
    );
    const view = render(panel(halfHour));
    expect(chartMock.setOption).toHaveBeenCalledTimes(1);
    expect(chartMock.setOption.mock.calls[0][0].xAxis[0].max).toBe(130_000);

    now.mockReturnValue(131_000);
    view.rerender(panel({ ...halfHour }));
    expect(chartMock.setOption).toHaveBeenCalledTimes(1);

    // Even an unchanged successful response must advance the rolling window at refresh time.
    now.mockReturnValue(140_000);
    act(() => client.setQueryData(queryKey, structuredClone(ping)));
    await waitFor(() => expect(chartMock.setOption).toHaveBeenCalledTimes(2));
    expect(chartMock.setOption.mock.calls[1][0]).toMatchObject({
      animation: false,
      xAxis: [{ max: 140_000 }, { max: 140_000 }],
    });

    client.setQueryData(['node-ping-probe', 'n1', '0-125'], ping);
    view.rerender(panel({ ...halfHour, startUnixSecs: 0, endUnixSecs: 125 }));
    expect(chartMock.setOption).toHaveBeenCalledTimes(3);
    expect(chartMock.setOption.mock.calls[2][0].xAxis[0].max).toBe(125_000);
    view.unmount();
    client.clear();
  });

  const pingSeries = (address: string, samples: PingProbePoint[]): PingProbeFamilySeries => ({ address, samples });
  const pingPanel = (ping: NodePingProbeView) => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    client.setQueryData(['node-ping-probe', 'n1', '0-130'], ping);
    const observationModules = { LoadCard, ThroughputChart, PingLatencyChart, ObservationChartLoading };
    const view = render(
      <QueryClientProvider client={client}>
        <PingProbePanel
          nodeId="n1"
          range={{ ...halfHour, startUnixSecs: 0, endUnixSecs: 130 }}
          linked={false}
          observationModules={observationModules}
        />
      </QueryClientProvider>,
    );
    return { view, client };
  };
  const lastChart = (name: string) =>
    [...chartMock.setOption.mock.calls].reverse().find(([option]) => option.series?.[0]?.name === name)?.[0];

  it('shows range loss rates in both Ping legends while preserving latency curves', () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const points = [
      { probed_at_unix_secs: 100, attempted: true, latency_us: 12_000 },
      { probed_at_unix_secs: 110, attempted: true, latency_us: null },
      { probed_at_unix_secs: 115, attempted: false, latency_us: null },
      { probed_at_unix_secs: 120, attempted: true, latency_us: 24_000 },
    ];
    const ipv4 = (name: string, kind: 'icmp' | 'tcp', address: string, samples: PingProbePoint[]) => ({
      name,
      kind,
      ipv4: pingSeries(address, samples),
      ipv6: null,
    });
    const ping: NodePingProbeView = {
      node_id: 'n1',
      targets: [
        ipv4('ICMP 混合', 'icmp', 'icmp://192.0.2.1', points),
        ipv4('ICMP 正常', 'icmp', 'icmp://192.0.2.2', [{ ...points[0], latency_us: 0 }]),
        ipv4('ICMP 空白', 'icmp', 'icmp://192.0.2.3', []),
        ipv4('TCP 无响应', 'tcp', 'tcp://192.0.2.1:443', [points[1], points[2]]),
        ipv4('TCP 未发包', 'tcp', 'tcp://192.0.2.2:443', [points[2]]),
      ],
    };
    client.setQueryData(['node-ping-probe', 'n1', '0-130'], ping);
    client.setQueryData(['node-ping-probe', 'n1', '116-130'], {
      ...ping,
      targets: ping.targets.map(target => ({
        ...target,
        ipv4: target.ipv4 && {
          ...target.ipv4,
          samples: target.ipv4.samples.filter(point => point.probed_at_unix_secs >= 116),
        },
      })),
    });
    const observationModules = { LoadCard, ThroughputChart, PingLatencyChart, ObservationChartLoading };
    const panel = (startUnixSecs: number) => (
      <QueryClientProvider client={client}>
        <PingProbePanel
          nodeId="n1"
          range={{ ...halfHour, startUnixSecs, endUnixSecs: 130 }}
          linked={false}
          observationModules={observationModules}
        />
      </QueryClientProvider>
    );
    const view = render(panel(0));
    expect(view.getByLabelText('ICMP 混合 丢包率 33.33%').textContent).toBe('33.33%');
    expect(view.getByLabelText('ICMP 正常 丢包率 0%').textContent).toBe('0%');
    expect(view.getByLabelText('ICMP 空白 丢包率 —').textContent).toBe('—');
    expect(view.getByLabelText('TCP 无响应 丢包率 100%').textContent).toBe('100%');
    expect(view.getByLabelText('TCP 未发包 丢包率 —').textContent).toBe('—');
    expect(view.getByLabelText('ICMP 混合 丢包率 33.33%').parentElement?.title).toContain('丢包 1 / 已探测 3');
    expect(view.getByLabelText('ICMP 混合 丢包率 33.33%').parentElement?.title).toContain('IPv4 icmp://192.0.2.1');
    // Partial loss and a target that answered nothing it was asked are told apart.
    expect(view.getByLabelText('ICMP 混合 丢包率 33.33%').className).toBe('loss partial');
    expect(view.getByLabelText('TCP 无响应 丢包率 100%').className).toBe('loss down');
    expect(view.getByLabelText('ICMP 正常 丢包率 0%').className).toBe('');
    const chart = lastChart('ICMP 混合');
    expect(chart.series[0].data).toEqual([
      [100_000, 12],
      [110_000, null],
      [115_000, null],
      [120_000, 24],
    ]);
    expect(chart.tooltip.formatter([{ axisValue: 120_000 }])).toContain('24 ms');
    view.rerender(panel(116));
    expect(view.getByLabelText('ICMP 混合 丢包率 0%').textContent).toBe('0%');
    expect(view.getByLabelText('TCP 无响应 丢包率 —').textContent).toBe('—');
    view.unmount();
    client.clear();
  });

  it('switches the whole Ping panel between address families from its corner switch', () => {
    const point = (at: number, latency_us: number | null): PingProbePoint => ({
      probed_at_unix_secs: at,
      attempted: true,
      latency_us,
    });
    const { view, client } = pingPanel({
      node_id: 'n1',
      targets: [
        {
          name: 'CF',
          kind: 'icmp',
          ipv4: pingSeries('icmp://1.1.1.1', [point(100, 10_000)]),
          ipv6: pingSeries('icmp://[2606:4700:4700::1111]', [point(100, 20_000), point(110, null)]),
        },
        { name: 'GitHub', kind: 'tcp', ipv4: pingSeries('tcp://github.com:443', [point(100, 30_000)]), ipv6: null },
      ],
    });

    // One switch for the panel, last in the first block's title bar; the second block ends with a
    // slot of the same width so both legends end at the same edge.
    const [switcher] = view.getAllByRole('radiogroup', { name: 'Ping 地址族' });
    const caps = view.container.querySelectorAll('.ping-probe-block > .load-network-cap');
    expect(view.getAllByRole('radiogroup', { name: 'Ping 地址族' })).toHaveLength(1);
    expect(caps[0].lastElementChild).toBe(switcher);
    expect(caps[1].lastElementChild?.className).toBe('ping-family-switch-spacer');
    const ipv6 = () => view.getByRole('radio', { name: 'IPv6' });
    expect(ipv6().getAttribute('aria-checked')).toBe('false');
    // The family not shown lost packets in the range: a dot, not a number.
    expect(ipv6().dataset.dot).toBe('partial');
    expect(lastChart('CF').series[0].data).toEqual([[100_000, 10]]);

    fireEvent.click(ipv6());

    expect(ipv6().getAttribute('aria-checked')).toBe('true');
    expect(ipv6().dataset.dot).toBeUndefined();
    expect(view.getByRole('radio', { name: 'IPv4' }).dataset.dot).toBeUndefined();
    expect(lastChart('CF').series[0].data).toEqual([
      [100_000, 20],
      [110_000, null],
    ]);
    expect(lastChart('CF').tooltip.formatter([{ axisValue: 110_000 }])).toContain('· IPv6');
    expect(view.getByLabelText('CF 丢包率 50%').className).toBe('loss partial');
    expect(view.getByText('没有填写 IPv6 地址的 TCP 目标。')).toBeTruthy();

    fireEvent.keyDown(switcher, { key: 'ArrowLeft' });
    expect(view.getByRole('radio', { name: 'IPv4' }).getAttribute('aria-checked')).toBe('true');
    view.unmount();
    client.clear();
  });

  it('keeps the Ping panel on the family the machine can probe', () => {
    const reply = { probed_at_unix_secs: 100, attempted: true, latency_us: 10_000 };
    const noRoute = pingPanel({
      node_id: 'n1',
      targets: [
        {
          name: 'CF',
          kind: 'icmp',
          ipv4: pingSeries('icmp://1.1.1.1', [reply]),
          ipv6: pingSeries('icmp://[2606:4700:4700::1111]', [
            { probed_at_unix_secs: 100, attempted: false, latency_us: null, skip_reason: 'no_route' },
          ]),
        },
      ],
    });
    const blocked = noRoute.view.getByRole('radio', { name: 'IPv6' }) as HTMLButtonElement;
    expect(blocked.disabled).toBe(true);
    expect(blocked.title).toBe('机器没有 IPv6 路由，IPv6 未探测');
    noRoute.view.unmount();
    noRoute.client.clear();

    const onlyIpv6 = pingPanel({
      node_id: 'n1',
      targets: [{ name: 'v6', kind: 'icmp', ipv4: null, ipv6: pingSeries('icmp://[2001:db8::1]', [reply]) }],
    });
    expect(onlyIpv6.view.getByRole('radio', { name: 'IPv6' }).getAttribute('aria-checked')).toBe('true');
    const ipv4 = onlyIpv6.view.getByRole('radio', { name: 'IPv4' }) as HTMLButtonElement;
    expect(ipv4.disabled).toBe(true);
    expect(ipv4.title).toBe('没有填写 IPv4 地址的目标');
    expect(lastChart('v6').series[0].data).toEqual([[100_000, 10]]);
    onlyIpv6.view.unmount();
    onlyIpv6.client.clear();
  });

  it('names why a series was never probed and colors 「+N」 by the loss it folds', () => {
    const reply = { probed_at_unix_secs: 100, attempted: true, latency_us: 10_000 };
    const lost = { probed_at_unix_secs: 100, attempted: true, latency_us: null };
    const { view, client } = pingPanel({
      node_id: 'n1',
      targets: [
        {
          name: '无 A 记录',
          kind: 'icmp',
          ipv4: pingSeries('icmp://v6only.example', [
            { probed_at_unix_secs: 100, attempted: false, latency_us: null, skip_reason: 'no_address' },
          ]),
          ipv6: null,
        },
        ...['乙', '丙', '丁', '戊'].map(name => ({
          name,
          kind: 'icmp' as const,
          ipv4: pingSeries(`icmp://${name}.example`, [name === '戊' ? lost : reply]),
          ipv6: null,
        })),
      ],
    });
    const skipped = view.getByLabelText('无 A 记录 未探测：域名没有 A 记录');
    expect(skipped.textContent).toBe('无 A');
    expect(skipped.className).toBe('gap');
    const more = view.container.querySelector('.ping-probe-more')!;
    expect(more.textContent).toBe('+2');
    expect(more.className).toBe('ping-probe-more down');
    expect(more.getAttribute('title')).toBe('未列出的目标有丢包：\n戊 100%');
    view.unmount();
    client.clear();
  });

  it('does not render the host summary strip', () => {
    const view = render(<LoadCard report={report()} />);

    expect(view.container.querySelector('.load-host-strip')).toBeNull();
    expect(view.queryByText(/个进程都正常/)).toBeNull();
  });

  it('formats multi-day uptime with spaced day and hour units only', () => {
    const value = reportWith(sample => {
      sample.uptime_secs = 15 * 86_400 + 2 * 3_600 + 11 * 60;
    });
    const view = render(<LoadCard report={value} />);

    expect(view.getByText('15 天 2 小时')).toBeTruthy();
    expect(view.queryByText(/15 天 2 小时 11 分钟/)).toBeNull();
  });

  it('places one-core utilization beside Cgroup throttling', () => {
    const value = reportWith(sample => {
      sample.cpu_detail!.cores = sample.cpu_detail!.cores.slice(0, 1);
    });
    const view = render(<LoadCard report={value} />);

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    const throttling = view.getByText('Cgroup 限流时间').closest('section');
    const oneCore = view.getByText('逐核繁忙度 · 1 核').closest('section');
    expect(throttling?.classList.contains('history-chart-card-wide')).toBe(false);
    expect(oneCore?.classList.contains('history-chart-card-wide')).toBe(false);
  });

  it('renders multi-core utilization as one ECharts line per core instead of a heatmap', () => {
    const view = render(<LoadCard report={report()} />);

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    const perCore = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === 'CPU 0'),
    )?.[0];
    expect(perCore).toBeTruthy();
    expect(perCore.series.map((line: { name: string }) => line.name)).toEqual(['CPU 0', 'CPU 1']);
    expect(view.getByLabelText('逐核繁忙度 · 2 核 图例').tagName).toBe('FOOTER');
    expect(view.container.querySelector('.core-heat-cells')).toBeNull();
  });

  it('uses the muted, straight, medium-fill history chart visual contract', () => {
    const view = render(<LoadCard report={report()} />);

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    const chart = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === '用户态'),
    )?.[0];
    expect(chart).toBeTruthy();
    expect(chart.animation).toBe(true);
    expect(chart.animationThreshold).toBe(10_000);
    expect(chart.animationDuration).toBe(360);
    expect(chart.animationEasing).toBe('cubicInOut');
    expect(chart.animationDurationUpdate).toBe(0);
    expect(chart.color.slice(0, 6)).toEqual(['#3e8fb0', '#e99cd3', '#8bbe95', '#7da1e3', '#ea9a97', '#9ccfd8']);
    expect(chart.xAxis[0].splitLine.show).toBe(true);
    expect(chart.yAxis.splitLine.show).toBe(true);
    expect(chart.grid).toMatchObject({ left: 10, containLabel: true });
    expect(chart.xAxis[0].axisLabel.fontSize).toBe(9.5);
    expect(chart.yAxis.axisLabel.fontSize).toBe(9.5);
    expect(chart.tooltip.extraCssText).toContain('box-shadow');

    for (const line of chart.series) {
      expect(line.smooth).toBe(false);
      expect(line.blendMode).toBeUndefined();
    }
    // 五段成分堆叠，互不重叠，取平涂；「窗口峰值」是画在成分之上的包络，只画线不填色。
    for (const line of chart.series.slice(0, 5)) {
      expect(line.stack).toBe('cpu');
      expect(line.areaStyle.opacity).toBe(1);
      expect(line.areaStyle.color).toMatch(/^rgba\(\d+,\d+,\d+,0\.34\)$/);
    }
    expect(chart.series[0].areaStyle.color).toBe('rgba(62,143,176,0.34)');
    const envelope = chart.series.find((line: { name: string }) => line.name === '窗口峰值');
    expect(envelope.stack).toBeUndefined();
    expect(envelope.areaStyle).toBeUndefined();

    const tooltip = chart.tooltip.formatter([
      { seriesName: '低值', color: '#111', value: [130_000, 1], dataIndex: 0 },
      { seriesName: '高值', color: '#222', value: [130_000, 2], dataIndex: 0 },
    ]);
    expect(tooltip.indexOf('高值')).toBeLessThan(tooltip.indexOf('低值'));

    const updated = report();
    updated.series = updated.series.map(entry => ({ ...entry, cpu_user_pct: entry.cpu_user_pct + 1 }));
    view.rerender(<LoadCard report={updated} />);
    const updatedChart = chartMock.setOption.mock.calls
      .filter(([option]) => option.series?.some((line: { name: string }) => line.name === '用户态'))
      .at(-1)?.[0];
    expect(updatedChart.animation).toBe(false);
    expect(updatedChart.animationDuration).toBe(0);
  });

  it('keeps old-agent KPI values but disables unavailable drill-downs', () => {
    const view = render(<LoadCard report={report(false)} />);
    expect(view.queryByText('CPU · 30 MINUTES')).toBeNull();
    expect((view.getByRole('button', { name: /CPU/ }) as HTMLButtonElement).disabled).toBe(true);
    expect((view.getByRole('button', { name: /内存/ }) as HTMLButtonElement).disabled).toBe(true);
    expect((view.getByRole('button', { name: /磁盘/ }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('renders all 60 windows and preserves raw gaps in every history series', () => {
    const series = Array.from({ length: 60 }, (_, index) => {
      const value = sample();
      value.window_start_unix_secs = 100 + index * 30;
      value.window_end_unix_secs = 130 + index * 30;
      value.cpu_detail!.pressure_some_pct = index === 8 ? 11.084 : 0.18 + index / 100;
      value.has_gap = index === 24;
      return value;
    });
    const value = report();
    value.reported_at_unix_secs = series[59].window_end_unix_secs;
    value.series = series;

    const view = render(<LoadCard report={value} />);
    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    expect(view.queryByText(/60 \/ 60 个 30 秒窗口/)).toBeNull();
    const pressure = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === 'CPU PSI some'),
    )?.[0];
    expect(pressure).toBeTruthy();
    expect(pressure.xAxis[0].type).toBe('value');
    const cpuPsi = pressure.series.find((line: { name: string }) => line.name === 'CPU PSI some');
    expect(cpuPsi.data).toHaveLength(60);
    expect(cpuPsi.data[8]).toEqual([(130 + 8 * 30) * 1000, 11.084]);
    expect(cpuPsi.data[24]).toEqual([(130 + 24 * 30) * 1000, null]);
  });

  it('shows extreme values as data without producing an automatic diagnosis', () => {
    const view = render(
      <LoadCard
        report={reportWith(sample => {
          const detail = sample.cpu_detail!;
          detail.pressure_some_pct = 18;
          detail.io_pressure_full_pct = 12;
          detail.throttled_usec = 50_000;
          sample.memory_detail!.available_min_bytes = 32 * 1024 ** 2;
          sample.memory_detail!.swap_out_bytes = 128 * 1024 ** 2;
        })}
      />,
    );
    expect(view.queryByText(/调度拥塞|容量危险|正在换出|已恢复/)).toBeNull();
    expect(view.getByRole('button', { name: /CPU/ }).classList.contains('bad')).toBe(false);
    expect(view.getByRole('button', { name: /CPU/ }).classList.contains('warn')).toBe(false);
  });

  it('keeps the full 24-hour absolute axis without synthesizing load slots', () => {
    const value = report();
    const view = render(<LoadCard report={value} historyLabel="24 HOURS" />);

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    expect(view.getByRole('region', { name: 'CPU 24 HOURS 数值' })).toBeTruthy();
    expect(view.queryByText('CPU · 24 HOURS')).toBeNull();
    expect(view.queryByText(/1 \/ 2,880 个 30 秒窗口/)).toBeNull();
    view.unmount();
    value.range_start_unix_secs = 1_000;
    value.range_end_unix_secs = 87_400;
    value.series[0].window_start_unix_secs = 10_000;
    value.series[0].window_end_unix_secs = 10_030;
    renderThroughput(value, false, {
      seconds: 24 * 60 * 60,
      label: '24h',
      menuLabel: '近 24 小时',
      heading: '24 HOURS',
    });
    const network = chartMock.setOption.mock.calls.find(([option]) =>
      option.series?.some((line: { name: string }) => line.name === '接收'),
    )?.[0];
    expect(network).toBeTruthy();
    expect(network.xAxis[0].type).toBe('value');
    expect(network.xAxis[0].min).toBe(1_000_000);
    expect(network.xAxis[0].max).toBe(87_400_000);
    expect(network.series[0].data).toEqual([[10_030_000, 100]]);
    expect(network.series[1].data).toEqual([[10_030_000, 200]]);
  });

  it('replaces stale throughput readings when the selected interval has no Agent samples', () => {
    const value = report();
    value.series = [];
    const view = renderThroughput(value);

    expect(view.getAllByText('尚无 Agent 上报样本。')).toHaveLength(2);
    expect(view.queryByText(/最后样本/)).toBeNull();
    expect(view.container.querySelector('.ndtp-ec')).toBeNull();
    expect(chartMock.setOption).not.toHaveBeenCalled();
  });

  it('does not treat a gap-marked last sample as a valid NIC rate', () => {
    const value = report();
    value.latest_sample!.has_gap = true;
    const view = renderThroughput(value);

    expect(view.getByLabelText('网卡流量图例').textContent).toContain('接收 —');
    expect(view.getByLabelText('网卡流量图例').textContent).toContain('发送 —');
  });
});
