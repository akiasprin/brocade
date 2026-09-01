import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { HostFacts, LoadSample, NodeLoadView } from '../src/api';

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
vi.mock('echarts/components', () => ({ GridComponent: {}, TooltipComponent: {} }));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

let LoadCard: typeof import('../src/panes/telemetry').LoadCard;

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

describe('deep host telemetry', () => {
  it('starts collapsed and switches between complete CPU, memory, disk and network histories', () => {
    const view = render(<LoadCard report={report()} />);
    expect(view.queryByText('CPU · 30 MINUTES')).toBeNull();
    expect(view.queryByText('MEMORY · 30 MINUTES')).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    expect(view.getByText('CPU · 30 MINUTES')).toBeTruthy();
    expect(view.getByText('CPU 时间占比')).toBeTruthy();
    expect(view.getByText('CPU 与 I/O 压力')).toBeTruthy();
    expect(view.getByText('负载与运行队列')).toBeTruthy();
    expect(view.getByText('逐核繁忙度 · 2 核')).toBeTruthy();
    expect(view.queryByText('CPU 频率')).toBeNull();
    expect(view.queryByText('MEMORY · 30 MINUTES')).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /内存/ }));
    expect(view.getByText('MEMORY · 30 MINUTES')).toBeTruthy();
    expect(view.getByText('容量构成')).toBeTruthy();
    expect(view.getByText('内核缓存与固定页')).toBeTruthy();
    expect(view.getByText('Swap、脏页与回写')).toBeTruthy();
    expect(view.getByText('缺页与直接回收')).toBeTruthy();
    expect(view.queryByText('CPU · 30 MINUTES')).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /内存/ }));
    expect(view.queryByText('MEMORY · 30 MINUTES')).toBeNull();

    fireEvent.click(view.getByRole('button', { name: /磁盘/ }));
    expect(view.getByText('DISK · 30 MINUTES')).toBeTruthy();
    expect(view.getByText('块设备吞吐')).toBeTruthy();
    expect(view.getByText('块设备 IOPS')).toBeTruthy();
    expect(view.getByText('完成延迟')).toBeTruthy();
    expect(view.getByText('设备繁忙与 I/O 压力')).toBeTruthy();
    expect(view.getByText('队列')).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: /连接表/ }));
    expect(view.getByText('NETWORK · 30 MINUTES')).toBeTruthy();
    expect(view.getByText('出站端口压力（估算）· 最繁忙目标')).toBeTruthy();
    expect(view.getByText('出站临时端口套接字')).toBeTruthy();
    expect(view.getByText('连接与套接字')).toBeTruthy();
    expect(view.getByText('TCP 连接生命周期')).toBeTruthy();
    expect(view.getByText('TCP 重传与异常')).toBeTruthy();
    expect(view.getByText('监听队列与 UDP 丢弃')).toBeTruthy();
    expect(view.getByText('套接字资源')).toBeTruthy();
    expect(view.queryByText('CPU · 30 MINUTES')).toBeNull();
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
    expect(view.getByText(/60 \/ 60 个 30 秒窗口/)).toBeTruthy();
    const pressure = chartMock.setOption.mock.calls.find(
      ([option]) =>
        option.xAxis?.data?.length === 60 &&
        option.series?.some((line: { name: string }) => line.name === 'CPU PSI some'),
    )?.[0];
    expect(pressure).toBeTruthy();
    const cpuPsi = pressure.series.find((line: { name: string }) => line.name === 'CPU PSI some');
    expect(cpuPsi.data).toHaveLength(60);
    expect(cpuPsi.data[8]).toBe(11.084);
    expect(cpuPsi.data[24]).toBeNull();
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
    const view = render(<LoadCard report={report()} historyLabel="24 HOURS" historyWindows={2_880} />);

    fireEvent.click(view.getByRole('button', { name: /CPU/ }));
    expect(view.getByText('CPU · 24 HOURS')).toBeTruthy();
    expect(view.getByText(/1 \/ 2,880 个 30 秒窗口/)).toBeTruthy();
    const network = chartMock.setOption.mock.calls.find(
      ([option]) =>
        option.xAxis?.data?.length === 2_880 && option.series?.some((line: { name: string }) => line.name === '接收'),
    )?.[0];
    expect(network).toBeTruthy();
  });
});
