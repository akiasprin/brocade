import { useMemo } from 'react';
import { useMetricQuery } from './metric-cadence';
import { freshFleetNicSample, useFleetRealtime, type FleetRealtimeNode } from '../fleet-realtime';
import { fetchNodeNicListWindows, type NodeNicView } from '../api';
import { Icon } from './icons';
import { observeBpsReading, observeBpsReadingParts } from './observe-chart';

// The crumb renders only the current aggregate. Historical curves belong to the machine page;
// asking for two hours here made every Console route download the same unused 120-window series.
const FLEET_TRAFFIC_WINDOWS = 1;
const FLEET_TRAFFIC_REFETCH_MS = 30_000;
const FLEET_TRAFFIC_FRESH_SECS = 90;

export interface FleetTrafficSummary {
  rxBps: number;
  txBps: number;
  peakBps: number;
  latestWindowEnd: number;
  sampledNodes: number;
  totalNodes: number;
}

/**
 * Build a fleet reading from each machine's newest 30-second window.
 *
 * The endpoint deliberately keeps an offline machine's final history so its card does not go
 * blank. That history must not be counted as current fleet throughput, hence the wall-clock
 * freshness bound here.
 */
export function summarizeFleetTraffic(nodes: NodeNicView[], nowUnixSecs: number): FleetTrafficSummary | null {
  const live = nodes.flatMap(node => {
    const latest = node.series.at(-1);
    if (!latest || latest.has_gap || nowUnixSecs - latest.window_end_unix_secs > FLEET_TRAFFIC_FRESH_SECS) return [];
    return [{ node, latest }];
  });
  if (live.length === 0) return null;

  let rxBps = 0;
  let txBps = 0;
  let latestWindowEnd = 0;
  for (const { latest } of live) {
    rxBps += latest.nic_rx_bps;
    txBps += latest.nic_tx_bps;
    latestWindowEnd = Math.max(latestWindowEnd, latest.window_end_unix_secs);
  }

  return {
    rxBps,
    txBps,
    peakBps: Math.max(rxBps, txBps),
    latestWindowEnd,
    sampledNodes: live.length,
    totalNodes: nodes.length,
  };
}

function realtimeSummary(
  nodes: Map<string, FleetRealtimeNode>,
  nowMillis: number,
  historical: FleetTrafficSummary | null,
): FleetTrafficSummary | null {
  let rxBps = 0;
  let txBps = 0;
  let latestWindowEnd = 0;
  let sampledNodes = 0;
  for (const node of nodes.values()) {
    const event = freshFleetNicSample(node, nowMillis);
    if (!event) continue;
    // The realtime protocol transports counter deltas in bytes/s. Historical NIC windows and
    // the presentation formatter use bits/s, so normalize before combining nodes.
    rxBps += event.sample.rx_bytes_per_sec * 8;
    txBps += event.sample.tx_bytes_per_sec * 8;
    latestWindowEnd = Math.max(latestWindowEnd, Math.floor(event.received_at_unix_millis / 1_000));
    sampledNodes += 1;
  }
  if (sampledNodes === 0) return null;
  return {
    rxBps,
    txBps,
    peakBps: Math.max(historical?.peakBps ?? 0, rxBps, txBps),
    latestWindowEnd,
    sampledNodes,
    totalNodes: Math.max(historical?.totalNodes ?? 0, nodes.size),
  };
}

function TrafficReading({ direction, value, reference }: { direction: 'rx' | 'tx'; value: number; reference: number }) {
  const label = direction === 'rx' ? '接收' : '发送';
  const reading = observeBpsReadingParts(value, reference);
  return (
    <span
      className="ft-rd"
      title={`${label} ${observeBpsReading(value)}`}
      aria-label={`${label} ${observeBpsReading(value)}`}
    >
      <Icon of={direction} size={12} className="ft-ic" />
      <span className="ft-rd-value">{reading.number}</span>
      <span className="ft-rd-unit">{reading.unit}</span>
    </span>
  );
}

function FleetTrafficReadout({ summary }: { summary: FleetTrafficSummary }) {
  const reference = Math.max(summary.rxBps, summary.txBps);
  return (
    <span className="ft-now">
      <TrafficReading direction="rx" value={summary.rxBps} reference={reference} />
      <TrafficReading direction="tx" value={summary.txBps} reference={reference} />
    </span>
  );
}

/** Fleet readout shared by every desktop Console page's top bar. */
export function FleetTrafficMeter() {
  const query = useMetricQuery({
    queryKey: ['node-nic-list', 'windows', FLEET_TRAFFIC_WINDOWS],
    queryFn: () => fetchNodeNicListWindows(FLEET_TRAFFIC_WINDOWS),
    refetchInterval: FLEET_TRAFFIC_REFETCH_MS,
    retry: false,
  });
  const summary = useMemo(
    () => summarizeFleetTraffic(query.data?.nodes ?? [], Math.floor(query.dataUpdatedAt / 1_000)),
    [query.data, query.dataUpdatedAt],
  );
  const realtime = useFleetRealtime();
  const realtimeReading = useMemo(
    () => realtimeSummary(realtime.nodes, realtime.nowMillis, summary),
    [realtime.nodes, realtime.nowMillis, summary],
  );
  const reading = realtimeReading ?? summary;

  if (query.error && !reading) {
    return (
      <span className="ft" role="status">
        <span className="ft-state bad">网卡读数不可用</span>
      </span>
    );
  }
  if (query.isPending && !reading) {
    return (
      <span className="ft" role="status">
        <span className="ft-state">
          <i className="ft-beat idle" aria-hidden="true" />
          正在读取整批流量
        </span>
      </span>
    );
  }
  if (!reading) {
    return (
      <span className="ft" role="status">
        <span className="ft-state">
          <i className="ft-beat idle" aria-hidden="true" />
          尚无实时网卡读数
        </span>
      </span>
    );
  }
  return (
    <span
      className="ft"
      aria-label="整批机器实时流量"
      title={`${realtimeReading ? '各机最近有效速率合计，非同一采样窗口' : '最近 30 秒窗口均值'} · ${reading.sampledNodes} / ${reading.totalNodes} 台有读数`}
    >
      <FleetTrafficReadout summary={reading} />
      {reading.sampledNodes < reading.totalNodes ? (
        <span className="ft-state">
          {reading.sampledNodes}/{reading.totalNodes}
        </span>
      ) : null}
    </span>
  );
}
