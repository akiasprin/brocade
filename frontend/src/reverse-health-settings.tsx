import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  DEFAULT_REVERSE_HEALTH,
  fetchSettings,
  saveSettings,
  reverseHealthError,
  type ReverseHealthPolicy,
  type ReverseHealthOverride,
} from './api';

const fields: [keyof ReverseHealthPolicy, string, number, number][] = [
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
function PolicyFields({ value, onChange }: { value: ReverseHealthPolicy; onChange: (p: ReverseHealthPolicy) => void }) {
  return (
    <div className="reverse-health-fields">
      {fields.map(([key, label, min, max]) => (
        <label key={key}>
          {label}
          <input
            type="number"
            min={min}
            max={max}
            value={Number.isNaN(value[key]) ? '' : value[key]}
            onChange={e => onChange({ ...value, [key]: e.target.value === '' ? Number.NaN : Number(e.target.value) })}
          />
        </label>
      ))}
    </div>
  );
}
export function ReverseHealthSettings() {
  const query = useQuery({ queryKey: ['settings'], queryFn: fetchSettings });
  const cache = useQueryClient();
  const [edit, setEdit] = useState<ReverseHealthPolicy | null>(null);
  const [overrides, setOverrides] = useState<ReverseHealthOverride[] | null>(null);
  const policy = edit ?? query.data?.reverse_health ?? DEFAULT_REVERSE_HEALTH;
  const rows = overrides ?? query.data?.reverse_health_overrides ?? [];
  const error =
    reverseHealthError(policy) ??
    rows
      .map(
        row =>
          reverseHealthError(row.health) ??
          (!row.chain || !row.from || !row.to || row.from === row.to ? '覆盖必须填写链路及不同的起点、终点' : null),
      )
      .find(Boolean);
  const mutation = useMutation({
    mutationFn: async () => {
      if (!query.data || error) throw new Error(error ?? '设置尚未加载');
      return saveSettings({ ...query.data, reverse_health: policy, reverse_health_overrides: rows });
    },
    onSuccess: () => {
      setEdit(null);
      setOverrides(null);
      cache.invalidateQueries({ queryKey: ['settings'] });
      cache.invalidateQueries({ queryKey: ['revisions'] });
    },
  });
  return (
    <section className="reverse-health-card" aria-label="反向隧道恢复设置">
      <h3>反向隧道：发现与恢复</h3>
      <p>
        所有在用和备用隧道持续双向探活。首次超时即停止分配新请求并补充连接；已有 TCP
        流不会自动重放。保存后需发布到链路两端。
      </p>
      <PolicyFields value={policy} onChange={setEdit} />
      {rows.map((row, i) => (
        <fieldset key={i}>
          <legend>定向链路覆盖 {i + 1}</legend>
          {(['chain', 'from', 'to'] as const).map((key, j) => (
            <label key={key}>
              {['链路 ID', '流量起点节点 ID', '流量终点节点 ID'][j]}
              <input
                value={row[key]}
                onChange={e =>
                  setOverrides(rows.map((r, index) => (index === i ? { ...r, [key]: e.target.value } : r)))
                }
              />
            </label>
          ))}
          <PolicyFields
            value={row.health}
            onChange={health => setOverrides(rows.map((r, index) => (index === i ? { ...r, health } : r)))}
          />
          <button type="button" onClick={() => setOverrides(rows.filter((_, index) => index !== i))}>
            恢复此链路的全局默认值
          </button>
        </fieldset>
      ))}
      <button
        type="button"
        onClick={() => setOverrides([...rows, { chain: '', from: '', to: '', health: { ...policy } }])}
      >
        添加定向覆盖
      </button>{' '}
      <button
        type="button"
        disabled={!query.data || !!error || mutation.isPending || (!edit && !overrides)}
        onClick={() => mutation.mutate()}
      >
        保存恢复设置
      </button>
      {error && <p role="alert">{error}</p>}
      {mutation.error && <p role="alert">{mutation.error.message}</p>}
      {mutation.isSuccess && <p>已保存修订 {mutation.data.revision_id}，等待发布。</p>}
    </section>
  );
}
