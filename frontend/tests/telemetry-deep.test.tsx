import { cleanup, fireEvent, render } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { HostFacts, LoadSample, NodeLoadView } from '../src/api';
import type { LoadRange } from '../src/panes/nodes';
import { observeAreaFill, observeTimeTick, observeValueAxis } from '../src/ui/observe-chart';

const chartMock = vi.hoisted(() => ({
  setOption: vi.fn(),
  connect: vi.fn(),
}));

vi.mock('echarts/core', () => ({
  use: vi.fn(),
  connect: chartMock.connect,
  init: vi.fn(() => ({ setOption: chartMock.setOption, resize: vi.fn(), dispose: vi.fn(), group: '' })),
}));
vi.mock('echarts/charts', () => ({ LineChart: {} }));
vi.mock('echarts/components', () => ({ GridComponent: {}, MarkLineComponent: {}, TooltipComponent: {} }));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

let LoadCard: typeof import('../src/panes/telemetry').LoadCard;
let ThroughputPanel: typeof import('../src/panes/nodes').ThroughputPanel;

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
  ({ LoadCard } = await import('../src/panes/telemetry'));
  ({ ThroughputPanel } = await import('../src/panes/nodes'));
});

afterEach(cleanup);

beforeEach(() => {
  chartMock.setOption.mockClear();
  chartMock.connect.mockClear();
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
  return {
    node_id: 'n1',
    reported_at_unix_secs: 130,
    clock_skew_secs: 0,
    host,
    series: [sample(deep)],
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
  it('uses seven time marks and advances the value ceiling by one standard tick', () => {
    const timeMarks = Array.from({ length: 60 }, (_, index) => index).filter(index => observeTimeTick(index, 60));
    expect(timeMarks).toEqual([0, 10, 20, 30, 39, 49, 59]);

    expect(observeValueAxis(873_410_000)).toEqual({ interval: 200_000_000, max: 1_000_000_000 });
    expect(observeValueAxis(500_000_000)).toEqual({ interval: 100_000_000, max: 600_000_000 });
    expect(observeValueAxis(100)).toEqual({ interval: 25, max: 125 });
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
    expect(chart.xAxis.min).toBe(firstEnd * 1000);
    expect(chart.xAxis.min % 60_000).not.toBe(0);
    expect(chart.xAxis.max).toBe((firstEnd + 90) * 1000);
  });

  it('keeps stacked bands flat and at full hue, since they never overlap', () => {
    expect(observeAreaFill('#286983', 'light', { stacked: true })).toBe('rgba(40,105,131,0.28)');
    expect(observeAreaFill('#3e8fb0', 'dark', { stacked: true })).toBe('rgba(62,143,176,0.34)');
  });

  it('never exposes the removed LOAD placeholder title when telemetry has no samples', () => {
    const value = report();
    value.series = [];
    const view = render(<LoadCard report={value} />);

    expect(view.queryByText('LOAD')).toBeNull();
    expect(view.getByText('还没有负载读数。')).toBeTruthy();
  });

  it('starts collapsed and switches between complete CPU, memory, disk and network histories', () => {
    const view = render(<LoadCard report={report()} />);
    expect(view.queryByRole('region', { name: 'CPU 30 MINUTES 数值' })).toBeNull();
    expect(view.queryByRole('region', { name: '内存 30 MINUTES 数值' })).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
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

  it('keeps the network throughput legend and metadata in the dedicated throughput panel', () => {
    const value = reportWith(sample => {
      sample.nic_rx_drop = 15;
    });
    const view = renderThroughput(value);
    const legend = view.getByLabelText('网卡流量图例');
    const header = view.getByText('网卡流量').parentElement;

    expect(legend.tagName).toBe('FOOTER');
    expect(view.container.querySelector('.ndtp-ec')).toBeTruthy();
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
    expect(chart.xAxis.splitLine.show).toBe(true);
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
    expect(chart.color.slice(0, 6)).toEqual(['#3e8fb0', '#e99cd3', '#8bbe95', '#7da1e3', '#ea9a97', '#9ccfd8']);
    expect(chart.xAxis.splitLine.show).toBe(true);
    expect(chart.yAxis.splitLine.show).toBe(true);
    expect(chart.grid).toMatchObject({ left: 10, containLabel: true });
    expect(chart.xAxis.axisLabel.fontSize).toBe(9.5);
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
    expect(pressure.xAxis.type).toBe('value');
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

  it('keeps the full 24-hour slot count in the network chart', () => {
    const value = report();
    const view = render(<LoadCard report={value} historyLabel="24 HOURS" historyWindows={2_880} />);

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    expect(view.getByRole('region', { name: 'CPU 24 HOURS 数值' })).toBeTruthy();
    expect(view.queryByText('CPU · 24 HOURS')).toBeNull();
    expect(view.queryByText(/1 \/ 2,880 个 30 秒窗口/)).toBeNull();
    view.unmount();
    renderThroughput(value, false, {
      seconds: 24 * 60 * 60,
      label: '24h',
      menuLabel: '近 24 小时',
      heading: '24 HOURS',
    });
    const network = chartMock.setOption.mock.calls.find(
      ([option]) =>
        option.series?.[0]?.data?.length === 2_880 &&
        option.series?.some((line: { name: string }) => line.name === '接收'),
    )?.[0];
    expect(network).toBeTruthy();
    expect(network.xAxis.type).toBe('value');
  });
});
