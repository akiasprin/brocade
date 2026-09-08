import {
  DEFAULT_REVERSE_HEALTH_TUNING,
  type ReverseHealthTuning,
  reverseHealthError,
  type ReverseHealthPolicy,
  type ReverseHealthOverride,
} from './api';

const fields: [Exclude<keyof ReverseHealthPolicy, 'tuning'>, string, number, number][] = [
  ['probe_interval_ms', '探活间隔（毫秒）', 100, 60000],
  ['probe_timeout_ms', '首次超时（毫秒）', 50, 10000],
  ['confirm_timeout_ms', '确认期限（毫秒）', 50, 10000],
  ['health_lease_ms', '健康证明有效期（毫秒）', 150, 120000],
  ['min_healthy_workers', '健康隧道下限', 1, 8],
  ['max_idle_ready_workers', '空闲隧道上限', 1, 16],
  ['max_parallel_dials_per_pair', '每条链路并行建连上限', 1, 8],
  ['dial_ready_timeout_ms', '建连与验证总期限（毫秒）', 200, 30000],
  ['reconnect_backoff_cap_ms', '重试退避上限（毫秒）', 250, 30000],
];
const tuningFields: [keyof ReverseHealthTuning, string, number, number][] = [
  ['probe_jitter_percent', '探活间隔抖动（百分比）', 0, 50],
  ['recovery_successes', '恢复所需连续应答次数', 1, 8],
  ['spare_workers', '忙时备用隧道数', 1, 8],
  ['max_healthy_workers', '健康隧道总数上限', 1, 32],
  ['max_sessions_per_worker', '每条隧道业务并发上限', 1, 256],
  ['reconnect_backoff_base_ms', '重试退避起点（毫秒）', 50, 30000],
  ['reconnect_stable_reset_ms', '稳定后重置退避（毫秒）', 1000, 300000],
  ['canary_interval_ms', '业务探测间隔（毫秒）', 100, 60000],
  ['canary_timeout_ms', '业务探测超时（毫秒）', 50, 30000],
  ['canary_successes', '业务稳定所需连续成功次数', 1, 1000],
  ['canary_stable_window_ms', '业务稳定最短观察期（毫秒）', 0, 300000],
];
function PolicyFields({ value, onChange }: { value: ReverseHealthPolicy; onChange: (p: ReverseHealthPolicy) => void }) {
  const tuning = value.tuning ?? DEFAULT_REVERSE_HEALTH_TUNING;
  return (
    <>
      <div className="reverse-health-fields">
        {fields.map(([key, label, min, max]) => (
          <label key={key}>
            {label}
            <input
              type="number"
              min={min}
              max={max}
              value={value[key] == null || Number.isNaN(value[key]) ? '' : value[key]}
              onChange={e => onChange({ ...value, [key]: e.target.value === '' ? Number.NaN : Number(e.target.value) })}
            />
          </label>
        ))}
      </div>
      <details>
        <summary>高级参数：探活、容量、退避与业务探测</summary>
        <p>
          健康上限约束可用池；排空中的旧隧道另行退出。恢复应答次数必须在确认期限内完成。退避在起点至上限之间增长，每次带
          50%–100% 抖动。
        </p>
        <div className="reverse-health-fields">
          {tuningFields.map(([key, label, min, max]) => (
            <label key={key}>
              {label}
              <input
                type="number"
                min={min}
                max={max}
                value={tuning[key] == null || Number.isNaN(tuning[key]) ? '' : tuning[key]}
                onChange={e =>
                  onChange({
                    ...value,
                    tuning: { ...tuning, [key]: e.target.value === '' ? Number.NaN : Number(e.target.value) },
                  })
                }
              />
            </label>
          ))}
        </div>
        <p>业务稳定需同时达到连续成功次数和最短观察期。业务探测地址沿用全局探测地址，未配置地址时不运行。</p>
      </details>
    </>
  );
}
export function reversePoliciesError(policy: ReverseHealthPolicy, rows: ReverseHealthOverride[]): string | null {
  const keys = rows.map(row => JSON.stringify([row.chain, row.from, row.to]));
  if (new Set(keys).size !== keys.length) return '同一定向链路不能重复覆盖';
  return (
    reverseHealthError(policy) ??
    rows
      .map(
        row =>
          reverseHealthError(row.health) ??
          (!row.chain.trim() || !row.from.trim() || !row.to.trim() || row.from === row.to
            ? '覆盖必须填写链路及不同的起点、终点'
            : null),
      )
      .find(Boolean) ??
    null
  );
}
export function ReverseHealthSettings({
  policy,
  rows,
  onPolicyChange,
  onOverridesChange,
}: {
  policy: ReverseHealthPolicy;
  rows: ReverseHealthOverride[];
  onPolicyChange: (p: ReverseHealthPolicy) => void;
  onOverridesChange: (rows: ReverseHealthOverride[]) => void;
}) {
  return (
    <section className="reverse-health-card" aria-label="反向隧道恢复设置">
      <h3>反向隧道：发现与恢复</h3>
      <p>
        所有在用和备用隧道持续双向探活。首次超时即停止分配新请求并补充连接；已有 TCP
        流不会自动重放。与连接策略一起保存后，需发布到链路两端。
      </p>
      <PolicyFields value={policy} onChange={onPolicyChange} />
      {rows.map((row, i) => (
        <fieldset key={i}>
          <legend>定向链路覆盖 {i + 1}</legend>
          {(['chain', 'from', 'to'] as const).map((key, j) => (
            <label key={key}>
              {['链路 ID', '流量起点节点 ID', '流量终点节点 ID'][j]}
              <input
                value={row[key]}
                onChange={e =>
                  onOverridesChange(rows.map((r, index) => (index === i ? { ...r, [key]: e.target.value } : r)))
                }
              />
            </label>
          ))}
          <PolicyFields
            value={row.health}
            onChange={health => onOverridesChange(rows.map((r, index) => (index === i ? { ...r, health } : r)))}
          />
          <button type="button" onClick={() => onOverridesChange(rows.filter((_, index) => index !== i))}>
            恢复此链路的全局默认值
          </button>
        </fieldset>
      ))}
      <button
        type="button"
        onClick={() => onOverridesChange([...rows, { chain: '', from: '', to: '', health: { ...policy } }])}
      >
        添加定向覆盖
      </button>{' '}
    </section>
  );
}
