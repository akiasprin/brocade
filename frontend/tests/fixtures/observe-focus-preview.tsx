import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { NodePingProbeView, PingProbeFamily, UsageNodeSeriesList } from '../../src/api';
import { LoadCard, ThroughputChart } from '../../src/panes/telemetry';
import { PingProbePanel, ThroughputPanel } from '../../src/panes/nodes';
import { ObservationChartLoading, PingLatencyChart } from '../../src/panes/node-observation-charts';
import { theme } from '../../src/forge/theme';
import { report as baseReport } from './ui-regression-data';
import '../../src/styles.css';

export { getInstanceByDom } from 'echarts/core';

const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const report = structuredClone(baseReport);
report.host = { ...report.host!, nic: 'eth0', nic_mtu: 1500, conntrack_max: 8192 };
report.series = report.series.map((sample, index) => {
  const wave = 1 + Math.sin(index / 5) * 0.3;
  return {
    ...sample,
    cpu_user_pct: 8 * wave,
    cpu_sys_pct: 4 * wave,
    cpu_peak_pct: index % 23 === 0 ? 42 : 18 * wave,
    cpu_detail: { ...sample.cpu_detail!, cores: sample.cpu_detail!.cores.slice(0, 2) },
    nic_rx_bps: (index % 19 === 0 ? 10 : wave) * 1_000_000,
    nic_tx_bps: 500_000 * (2 - wave),
    memory_detail: {
      available_min_bytes: 3 * 1024 ** 3,
      free_bytes: 2 * 1024 ** 3,
      anon_bytes: 2.4 * 1024 ** 3 * wave,
      file_cache_bytes: (4 - 2.4 * wave) * 1024 ** 3,
      shmem_bytes: 128 * 1024 ** 2,
      kernel_other_bytes: 1.875 * 1024 ** 3,
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
    },
    disk_detail: {
      total_bytes: 20 * 1024 ** 3,
      inode_total: 1_000_000,
      inode_free: 900_000,
      read_bps: 2 * 1024 ** 2 * wave,
      write_bps: 1024 ** 2 * (2 - wave),
      read_iops: 20 * wave,
      write_iops: 10,
      read_await_ms: 1.2,
      write_await_ms: 2.4,
      busy_pct: 12 * wave,
      queue_depth: 0.3,
      in_flight: 2,
      pressure_some_pct: 0.2,
      pressure_full_pct: 0,
    },
    network_detail: {
      tcp_curr_estab: 40 * wave,
      tcp_inuse: 95,
      tcp_time_wait: 20,
      tcp_orphan: 0,
      tcp_alloc: 142,
      tcp_mem_bytes: 512 * 1024,
      udp_inuse: 10,
      udp_mem_bytes: 64 * 1024,
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
    },
  };
});
report.latest_sample = report.series.at(-1)!;
const range = {
  seconds: 3600,
  label: '1h',
  menuLabel: '近 1 小时',
  heading: '1 HOUR',
  startUnixSecs: report.range_start_unix_secs,
  endUnixSecs: report.range_end_unix_secs,
};
const rangeKey = `${range.startUnixSecs}-${range.endUnixSecs}`;
const usage: UsageNodeSeriesList = {
  since: '',
  month_start: '',
  nodes: [
    {
      node_id: report.node_id,
      buckets: report.series.map((sample, index) => ({
        window_end: new Date(sample.window_end_unix_secs * 1000).toISOString(),
        user_uplink_bytes: (1 + Math.sin(index / 5) * 0.3) * 1_000_000,
        user_downlink_bytes: 2_000_000,
        relay_uplink_bytes: (index % 17) * 100_000,
        relay_downlink_bytes: 100_000,
      })),
      forwarding: report.series.map((sample, index) => ({
        window_start: new Date(sample.window_start_unix_secs * 1000).toISOString(),
        window_end: new Date(sample.window_end_unix_secs * 1000).toISOString(),
        uplink_bytes: (1 + Math.cos(index / 5) * 0.3) * 500_000,
        downlink_bytes: 500_000,
        has_gap: false,
      })),
      month_user_uplink_bytes: 1024 ** 3,
      month_user_downlink_bytes: 2 * 1024 ** 3,
      month_relay_uplink_bytes: 0,
      month_relay_downlink_bytes: 0,
      month_has_gap: false,
    },
  ],
};
const ping: NodePingProbeView = {
  node_id: report.node_id,
  targets: (['icmp', 'tcp'] as const).flatMap(kind =>
    ['广东电信', '广东联通', '广东移动'].map((name, targetIndex) => {
      const family = (family: PingProbeFamily) => ({
        address: `${kind}://${family === 'ipv4' ? `192.0.2.${targetIndex + 1}` : `[2001:db8::${targetIndex + 1}]`}`,
        samples: report.series.map((sample, index) => ({
          probed_at_unix_secs: sample.window_end_unix_secs,
          attempted: true,
          latency_us:
            targetIndex < 2 && index % (7 + targetIndex) === 0
              ? null
              : (targetIndex === 2 ? 20 : 320 + targetIndex * 15) * 1000,
        })),
      });
      return { name, kind, ipv4: family('ipv4'), ipv6: family('ipv6') };
    }),
  ),
};
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['node-load-history', report.node_id, rangeKey], report);
client.setQueryData(['usage-node-series', report.node_id, rangeKey], usage);
client.setQueryData(['node-ping-probe', report.node_id, rangeKey], ping);
const observationModules = { LoadCard, ThroughputChart, PingLatencyChart, ObservationChartLoading };
createRoot(document.getElementById('root')!).render(
  <main>
    <LoadCard report={report} historyLabel="1 HOUR" />
    <QueryClientProvider client={client}>
      <div className="nd-observe-throughput" style={{ marginTop: 16 }}>
        <ThroughputPanel nodeId={report.node_id} range={range} linked={false} observationModules={observationModules} />
        <PingProbePanel nodeId={report.node_id} range={range} linked={false} observationModules={observationModules} />
      </div>
    </QueryClientProvider>
  </main>,
);
