import { useEffect, useState } from 'react';
interface Worker {
  active_sessions?: number;
  affected_sessions?: number;
  control_queue_depth?: number;
  queue_delay_ms?: number;
  scheduler_lag_ms?: number;
  worker_id: string;
  pair: string;
  role: string;
  state: string;
  reason: string;
  ack_age_ms: number;
  rtt_ms: number;
  probes: number;
  acks: number;
  timeouts: number;
}
interface Transition extends Worker {
  sequence: number;
  at_unix_ms: number;
  from: string;
}
interface Canary {
  freshness_budget_ms?: number;
  pair: string;
  state: string;
  reason: string;
  latency_ms: number;
  consecutive_successes: number;
  stable_since_unix_ms: number;
  sampled_at_unix_ms: number;
}
interface Report {
  canaries?: Canary[];
  boot_id: string;
  sequence: number;
  sampled_at_unix_ms: number;
  workers: Worker[];
  events: Transition[];
}
interface Sample {
  received_at_unix_millis: number;
  sample: { sampled_at_unix_millis: number; reverse_health?: Report | null };
}
export function ReverseHealthCard({ nodeId }: { nodeId: string }) {
  const [last, setLast] = useState<{ event: Sample; arrived: number; nodeId: string } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [connected, setConnected] = useState(false);
  useEffect(() => {
    if (typeof EventSource === 'undefined') return;
    let source: EventSource;
    const accept = (event: Sample) => {
      setLast({ event, arrived: Date.now(), nodeId });
      setConnected(true);
    };
    const open = () => {
      source = new EventSource(`/realtime/nodes/${encodeURIComponent(nodeId)}/events`, { withCredentials: true });
      source.addEventListener('sample', e => {
        try {
          accept(JSON.parse((e as MessageEvent).data));
        } catch {
          setConnected(false);
        }
      });
      source.addEventListener('snapshot', e => {
        try {
          const nodes = JSON.parse((e as MessageEvent).data).nodes as { samples: Sample[] }[];
          const sample = nodes[0]?.samples.at(-1);
          if (sample) accept(sample);
        } catch {
          setConnected(false);
        }
      });
      source.addEventListener('status', e => {
        try {
          setConnected(Boolean(JSON.parse((e as MessageEvent).data).connected));
        } catch {
          setConnected(false);
        }
      });
      source.addEventListener('reset', () => {
        source.close();
        setConnected(false);
        open();
      });
      source.onerror = () => setConnected(false);
    };
    open();
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => {
      source.close();
      clearInterval(timer);
    };
  }, [nodeId]);
  const report = last?.nodeId === nodeId ? last.event.sample.reverse_health : undefined;
  // Compare node timestamps only with node timestamps; browser freshness uses receipt time.
  const nodeAge = report && last ? last.event.sample.sampled_at_unix_millis - report.sampled_at_unix_ms : Infinity;
  const stale =
    !connected ||
    !report ||
    !last ||
    now - last.arrived > 15000 ||
    now - last.event.received_at_unix_millis > 15000 ||
    nodeAge > 15000;
  const groups = new Map<string, Worker[]>();
  for (const w of report?.workers ?? []) {
    const key = `${w.pair} · ${w.role}`;
    groups.set(key, [...(groups.get(key) ?? []), w]);
  }
  return (
    <section className="reverse-health-card" aria-label="反向隧道健康">
      <h3>
        反向隧道健康 <small>{stale ? 'UNKNOWN · 数据未到达或已过期' : '实时'}</small>
      </h3>
      <p>隧道 ACK、可分配容量与业务探测分别显示。业务探测按链路配置的周期，通过实际反向路由访问探测地址。</p>
      {(report?.canaries ?? []).map(c => (
        <p key={c.pair}>
          <b>{c.pair} 业务探测：</b>
          {stale ||
          (report?.sampled_at_unix_ms ?? 0) - c.sampled_at_unix_ms >
            ((c.freshness_budget_ms ?? 0) > 0 ? c.freshness_budget_ms! : 15000)
            ? 'UNKNOWN'
            : c.state}{' '}
          · {c.latency_ms} ms · 连续成功 {c.consecutive_successes} 次{' '}
          {c.stable_since_unix_ms > 0
            ? `· 持续恢复起点 ${new Date(c.stable_since_unix_ms).toLocaleTimeString()}`
            : '· 尚未确认持续恢复'}
        </p>
      ))}
      {!stale && groups.size === 0 && <p>当前没有反向隧道。</p>}
      {Array.from(groups, ([name, workers]) => (
        <div key={name}>
          <b>{name}</b>
          <p>
            {stale
              ? '可用数量未知'
              : `可用 ${workers.filter(w => w.state === 'READY').length} / 总数 ${workers.length}`}
          </p>
          <table>
            <thead>
              <tr>
                <th>隧道</th>
                <th>状态</th>
                <th>RTT</th>
                <th>上次 ACK</th>
                <th>业务流</th>
                <th>控制排队 / 调度延迟</th>
                <th>超时次数</th>
              </tr>
            </thead>
            <tbody>
              {workers.map(w => (
                <tr key={w.worker_id}>
                  <td>{w.worker_id.slice(-8)}</td>
                  <td>{stale ? 'UNKNOWN' : w.state}</td>
                  <td>{w.rtt_ms} ms</td>
                  <td>{w.ack_age_ms < 0 ? '尚未确认' : `${w.ack_age_ms} ms 前`}</td>
                  <td>{w.active_sessions ?? 0}</td>
                  <td>
                    {w.queue_delay_ms ?? 0} / {w.scheduler_lag_ms ?? 0} ms
                  </td>
                  <td>{w.timeouts}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ))}
      <details>
        <summary>最近故障与恢复事件</summary>
        {(report?.events ?? [])
          .slice(-32)
          .reverse()
          .map(e => (
            <p key={`${report?.boot_id}:${e.sequence}`}>
              <time>{new Date(e.at_unix_ms).toLocaleTimeString()}</time> · {e.pair} · {e.worker_id.slice(-8)} · {e.from}{' '}
              → {e.state} · {e.reason}
              {e.affected_sessions ? ` · 受影响业务流 ${e.affected_sessions}` : ''}
            </p>
          ))}
      </details>
    </section>
  );
}
