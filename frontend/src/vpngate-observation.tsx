import { useNodeRealtime } from './node-realtime';
import { PanelTitle } from './ui/icons';
import { freshVpngateReport } from './vpngate-realtime';
const labels: Record<string, string> = {
  pending: '等待候选',
  healthy: '健康',
  degraded: '退化',
  failing_over: '切换中',
  failed: '失败',
  active: '主用',
  standby: '备用',
  starting: '启动中',
  unhealthy: '不可用',
  backoff: '退避',
  process_exited: '进程退出',
  socks_unavailable: 'SOCKS 不可达',
  egress_unreachable: '出口黑洞',
  candidate_removed: '候选移除',
  admission_rejected: '准入拒绝',
  start_failed: '启动失败',
  no_candidate: '无可用候选',
  active_failed: '主用失效',
  failover_started: '开始切换',
  failover_completed: '切换完成',
  standby_lost: '备用失效',
  refill_started: '开始补位',
  refill_completed: '补位完成',
  refill_failed: '补位失败',
  pool_recovered: '池已恢复',
};

const label = (value: string | undefined) => (value ? (labels[value] ?? value) : '—');
const age = (millis: number | undefined) =>
  millis === undefined ? '—' : millis < 1000 ? '<1 秒' : `${Math.floor(millis / 1000)} 秒`;
const clock = (unixMillis: number) =>
  new Date(unixMillis).toLocaleTimeString('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });

export function VpngateObservationCard({ nodeId }: { nodeId: string }) {
  const realtime = useNodeRealtime(nodeId);
  const report = realtime.last?.event.sample.vpngate;
  if (!report || report.pools.length === 0) return null;
  const stale = !freshVpngateReport(realtime);
  const healthy = report.pools.filter(pool => pool.state === 'healthy').length;
  const standbys = report.pools.reduce((total, pool) => total + pool.ready_standbys, 0);
  const troubled = report.pools.filter(pool => pool.state !== 'healthy');
  const backends = (outboundId: string) => report.backends.filter(backend => backend.outbound_id === outboundId);

  return (
    <div className="panel ov-card" aria-label="VPN Gate 实时路径">
      <header>
        <PanelTitle of="outbound">VPN Gate 实时路径</PanelTitle>
        <span className="sp" />
        <span className="hint">5 秒探活 · 最近 32 条切换事件</span>
      </header>

      <p className="rvh-total">
        <span className={stale ? 'lamp' : troubled.length > 0 ? 'lamp warn' : 'lamp ok'} />
        <span>
          出口池 <b>{report.pools.length}</b> · 健康 <b>{stale ? '?' : healthy}</b> · 就绪备用{' '}
          <b>{stale ? '?' : standbys}</b>
        </span>
        <span className="rvh-tail">{stale ? '数据已过期' : '计数自本次 Agent 启动'}</span>
      </p>

      {stale && <p className="rt-dx bad">实时数据超过 15 秒未更新，以下只代表最近一次快照。</p>}

      {report.pools.map(pool => (
        <details
          className={`rvh-fold rvh-sub ov-one${pool.state === 'healthy' ? '' : ' v-warn'}`}
          key={pool.outbound_id}
        >
          <summary>
            <span
              className={
                pool.state === 'healthy' && !stale ? 'lamp ok' : pool.state === 'failed' ? 'lamp bad' : 'lamp warn'
              }
            />
            <b>{pool.country_code}</b>
            <span className="mono peer">{pool.outbound_id}</span>
            <span className="nums">
              {label(pool.state)} · 备用 {pool.ready_standbys} · 候选 {pool.candidate_count}
            </span>
          </summary>
          <div className="ov-onebody">
            <p className="rt-dx">
              最近成功 {age(pool.last_success_age_millis)}前 · 连续失败 {pool.consecutive_failures} · 原因{' '}
              <b>{label(pool.reason)}</b>
            </p>
            <p className="rt-dx">
              探活 {pool.probes} / 失败 {pool.probe_failures} · 主备切换 {pool.failovers} · 补位 {pool.refill_attempts}{' '}
              / 失败 {pool.refill_failures}
              {pool.refill_backoff_remaining_millis > 0 ? ` · ${age(pool.refill_backoff_remaining_millis)}后重试` : ''}
            </p>
            <table className="t">
              <thead>
                <tr>
                  <th>角色</th>
                  <th>槽位</th>
                  <th>候选</th>
                  <th>状态</th>
                  <th>最近成功</th>
                  <th>连续失败</th>
                </tr>
              </thead>
              <tbody>
                {backends(pool.outbound_id).map(backend => (
                  <tr key={backend.slot}>
                    <td>{label(backend.role)}</td>
                    <td>{backend.slot}</td>
                    <td className="mono">{backend.server_id}</td>
                    <td>
                      {label(backend.state)}
                      {backend.reason ? ` · ${label(backend.reason)}` : ''}
                    </td>
                    <td>{age(backend.last_success_age_millis)}</td>
                    <td>{backend.consecutive_failures}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </details>
      ))}

      <details className="rvh-fold">
        <summary>状态事件 · 最近 {report.events.length} 条</summary>
        {report.events.length === 0 ? (
          <p className="rt-dx">本次 Agent 启动后还没有切换或补位事件。</p>
        ) : (
          <table className="t">
            <thead>
              <tr>
                <th>时间</th>
                <th>出口池</th>
                <th>事件</th>
                <th>切换</th>
                <th>原因 / 恢复耗时</th>
              </tr>
            </thead>
            <tbody>
              {[...report.events].reverse().map(event => (
                <tr key={event.sequence}>
                  <td>{clock(event.at_unix_millis)}</td>
                  <td className="mono">{event.outbound_id}</td>
                  <td>{label(event.kind)}</td>
                  <td>
                    {event.from_slot === undefined && event.to_slot === undefined
                      ? '—'
                      : `${event.from_slot ?? '—'} → ${event.to_slot ?? '—'}`}
                  </td>
                  <td>
                    {event.reason
                      ? label(event.reason)
                      : event.recovery_elapsed_millis === undefined
                        ? '—'
                        : `${event.recovery_elapsed_millis} ms`}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </details>
    </div>
  );
}
