import { lazy, Suspense, useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { fetchNodeLoadList, fetchUsageNodeSeries, type LoadSample, type UsageNodeBucket } from '../api';
import { Loading } from '../ui/bits';
import { PanelTitle } from '../ui/icons';
import { observeBpsUnit, observeValueAxis } from '../ui/observe-chart';
import type { FleetNetPoint } from './node-observation-charts';

const FleetNetChart = lazy(() =>
  import('./node-observation-charts').then(module => ({ default: module.FleetNetChart })),
);

const FLEET_WINDOWS = 240;
const FLEET_SECS = FLEET_WINDOWS * 30;
const GRID_SECS = 30;

function fetchFleetLoad() {
  const endUnixSecs = Math.floor(Date.now() / 1000);
  return fetchNodeLoadList(endUnixSecs - FLEET_SECS, endUnixSecs);
}

/** Two-hour fleet NIC throughput compared with the Xray bytes it carries. */
export function FleetNetPanel() {
  const load = useQuery({
    queryKey: ['node-load-list', FLEET_WINDOWS],
    queryFn: fetchFleetLoad,
    refetchInterval: 30_000,
    retry: false,
  });
  const usage = useQuery({
    queryKey: ['usage-node-series', FLEET_SECS],
    queryFn: () => fetchUsageNodeSeries(FLEET_SECS),
    refetchInterval: 30_000,
  });

  const geo = useMemo(() => {
    // Each Agent closes its 30-second window on a different phase. Put them on one grid before
    // adding them, otherwise exact timestamps omit most machines from every point.
    const acc = new Map<number, FleetNetPoint>();
    const at = (key: number): FleetNetPoint => {
      let point = acc.get(key);
      if (!point) {
        point = { nicRx: 0, nicTx: 0, xrayRx: 0, xrayTx: 0 };
        acc.set(key, point);
      }
      return point;
    };
    for (const node of load.data?.nodes ?? []) {
      for (const sample of node.series as LoadSample[]) {
        if (sample.has_gap) continue;
        const key = Math.floor(sample.window_end_unix_secs / GRID_SECS) * GRID_SECS;
        const point = at(key);
        point.nicRx += sample.nic_rx_bps;
        point.nicTx += sample.nic_tx_bps;
      }
    }
    for (const node of usage.data?.nodes ?? []) {
      for (const bucket of node.buckets as UsageNodeBucket[]) {
        const key = Math.floor(Date.parse(bucket.window_end) / 1000 / GRID_SECS) * GRID_SECS;
        const point = at(key);
        point.xrayRx += ((bucket.user_downlink_bytes + bucket.relay_downlink_bytes) * 8) / GRID_SECS;
        point.xrayTx += ((bucket.user_uplink_bytes + bucket.relay_uplink_bytes) * 8) / GRID_SECS;
      }
    }
    const allKeys = [...acc.keys()].sort((left, right) => left - right);
    if (allKeys.length === 0) return null;
    const endKey = allKeys[allKeys.length - 1];
    const startKey = Math.max(allKeys[0], endKey - (FLEET_WINDOWS - 1) * GRID_SECS);
    const count = Math.round((endKey - startKey) / GRID_SECS) + 1;
    const times: number[] = [];
    const points: FleetNetPoint[] = [];
    for (let index = 0; index < count; index += 1) {
      const key = startKey + index * GRID_SECS;
      times.push(key);
      points.push(acc.get(key) ?? { nicRx: 0, nicTx: 0, xrayRx: 0, xrayTx: 0 });
    }
    let peak = 1;
    for (const point of points) peak = Math.max(peak, point.nicRx, point.nicTx, point.xrayRx, point.xrayTx);
    return { pts: points, times, peak };
  }, [load.data, usage.data]);

  const unitName = geo ? observeBpsUnit(observeValueAxis(geo.peak)).name : null;
  const head = (
    <header>
      <PanelTitle of="usage">网络吞吐 · 全部机器</PanelTitle>
      {unitName && <span className="chart-unit">({unitName})</span>}
      <span className="sp" />
      <span className="hint">网卡汇总 对 XRAY 承载 · 接收在上 / 发送在下 · 近 2 小时</span>
    </header>
  );

  if (!load.data) return null;
  if (!geo) {
    return (
      <div className="panel titled ndnet">
        {head}
        <p className="note">还没有网络读数。</p>
      </div>
    );
  }

  return (
    <div className="panel titled ndnet">
      {head}
      <Suspense fallback={<Loading variant="chart" />}>
        <FleetNetChart times={geo.times} pts={geo.pts} peak={geo.peak} />
      </Suspense>
      <div className="ndnet-legend">
        <span>
          <i className="sw rx" />
          接收
        </span>
        <span>
          <i className="sw tx" />
          发送
        </span>
        <span className="vr" />
        <span>
          <i className="sw solid" />
          XRAY 承载
        </span>
        <span>
          <i className="sw out" />
          NIC 网卡
        </span>
        <span className="tail">缝隙 = 封装 / 系统开销</span>
      </div>
    </div>
  );
}
