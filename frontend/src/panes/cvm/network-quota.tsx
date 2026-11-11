import { useQuery } from '@tanstack/react-query';
import { useEffect, useMemo, useState } from 'react';
import {
  previewCvmNetworkQuota,
  type CvmHost,
  type CvmHostNetworkQuota,
  type CvmHostNetworkQuotaChange,
  type CvmNetworkQuotaLimits,
} from '../../api';
import { ErrorBox } from '../../ui/bits';
import { capacity } from './queries';

const integer = (n: number) => (Number.isFinite(n) ? n.toLocaleString('zh-CN') : '—');
export function networkQuotaStatus(quota: CvmHostNetworkQuota): string {
  const r = quota.report;
  if (r?.revision === quota.revision && r.error) return `执行失败：${r.error}`;
  if (!quota.fresh || !r?.applied || r.revision !== quota.revision) return '待生效';
  if (quota.held_states > quota.limits.max_states) return '收敛中：现有状态保留，新增受限';
  return quota.limits.max_states === 0 ? '已生效 · 暂停新增' : '已生效';
}

export function NetworkQuotaEditor({
  host,
  tenant,
  user,
  existing,
  onChange,
}: {
  host: CvmHost;
  tenant: string;
  user: string;
  existing: CvmHostNetworkQuota | undefined;
  onChange: (node: string, change: CvmHostNetworkQuotaChange | null, blocked: boolean) => void;
}) {
  const initial = useMemo(
    () =>
      existing?.limits ?? {
        max_states: Math.min(
          100000,
          Math.max(
            0,
            (host.capabilities.network_quota?.total_states ?? 0) -
              (host.capabilities.network_quota?.system_reserved_states ?? 0),
          ),
        ),
        new_per_second: 1000,
        burst: 2000,
      },
    [existing, host.capabilities.network_quota],
  );
  const [limits, setLimits] = useState<CvmNetworkQuotaLimits>(initial);
  const [debounced, setDebounced] = useState(limits);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(limits), 250);
    return () => clearTimeout(timer);
  }, [limits]);
  const supported = !!host.capabilities.network_quota;
  const valid =
    Number.isSafeInteger(limits.max_states) &&
    limits.max_states >= 0 &&
    limits.max_states <= 1048576 &&
    Number.isSafeInteger(limits.new_per_second) &&
    limits.new_per_second > 0 &&
    limits.new_per_second <= 100000 &&
    Number.isSafeInteger(limits.burst) &&
    limits.burst > 0 &&
    limits.burst <= 200000;
  const preview = useQuery({
    queryKey: ['cvm-network-quota-preview', tenant, user, host.node_id, debounced],
    queryFn: () =>
      previewCvmNetworkQuota({ tenant_id: tenant, user_id: user, node_id: host.node_id, limits: debounced }),
    enabled: supported && host.fresh && valid && limits === debounced,
    retry: false,
    staleTime: 0,
  });
  const current =
    preview.data &&
    preview.data.requested.max_states === limits.max_states &&
    preview.data.requested.new_per_second === limits.new_per_second &&
    preview.data.requested.burst === limits.burst
      ? preview.data
      : undefined;
  const changed =
    !existing ||
    limits.max_states !== existing.limits.max_states ||
    limits.new_per_second !== existing.limits.new_per_second ||
    limits.burst !== existing.limits.burst;
  useEffect(() => {
    const blocked = supported && changed && (!valid || !host.fresh || !current?.fits || preview.isFetching);
    onChange(
      host.node_id,
      supported && changed && current?.fits && valid
        ? {
            node_id: host.node_id,
            expected_revision: current.revision,
            budget_revision: current.budget_revision,
            limits,
          }
        : null,
      blocked,
    );
  }, [supported, changed, valid, host.fresh, host.node_id, current, limits, preview.isFetching, onChange]);
  const patch = (key: keyof CvmNetworkQuotaLimits, raw: string) => {
    setLimits(value => ({ ...value, [key]: raw === '' ? Number.NaN : Number(raw) }));
  };
  if (!supported) return <p className="note">网络额度尚不可用：宿主需要上报匹配的执行能力。</p>;
  return (
    <section className="cvm-network-quota" aria-label={`${host.name} 网络额度`}>
      <label>
        网络连接额度 <span className="note">条</span>
        <input
          className="f"
          type="number"
          min={0}
          max={1048576}
          step={1}
          aria-label={`${host.name} 网络连接额度`}
          value={Number.isFinite(limits.max_states) ? limits.max_states : ''}
          onChange={event => patch('max_states', event.target.value)}
        />
      </label>
      <p className="note">此用户在这台宿主上的全部容器共享。</p>
      <div className="cvm-quota-formula" role="status" aria-live="polite">
        {!host.fresh ? (
          '宿主能力已过期，等待重新核验。'
        ) : current ? (
          <>
            <strong>最多可设 {integer(current.maximum)} 条</strong>
            <span>
              计算：宿主可分配 {integer(current.capacity.total_states - current.capacity.system_reserved_states)} −
              其他用户已分配 {integer(current.other_allocated)} = {integer(current.maximum)} 条
            </span>
            {!current.fits && <span className="error-text">超出可分配额度 {integer(current.excess_states)} 条。</span>}
          </>
        ) : (
          '正在核验可分配额度…'
        )}
      </div>
      {!valid && <p role="alert">请输入有效整数。连接额度为 0 时暂停新增，已有状态正常保留。</p>}
      {limits.max_states === 0 && <p className="note">暂停新增连接，已有状态正常保留至关闭或超时。</p>}
      {existing && <p className="note">当前状态：{networkQuotaStatus(existing)}</p>}
      <div className="cvm-quota-disclosures">
        <details>
          <summary>高级设置</summary>
          <div className="cvm-form-grid">
            <label>
              持续新建速率 <span className="note">条/秒</span>
              <input
                className="f"
                type="number"
                min={1}
                max={100000}
                step={1}
                value={Number.isFinite(limits.new_per_second) ? limits.new_per_second : ''}
                onChange={event => patch('new_per_second', event.target.value)}
              />
            </label>
            <label>
              突发新建额度 <span className="note">条</span>
              <input
                className="f"
                type="number"
                min={1}
                max={200000}
                step={1}
                value={Number.isFinite(limits.burst) ? limits.burst : ''}
                onChange={event => patch('burst', event.target.value)}
              />
            </label>
          </div>
          <p className="note">
            任意 t 秒内新建数量 ≤ {integer(limits.new_per_second)} × t + {integer(limits.burst)}；突发仍受连接总额约束。
          </p>
        </details>
        <details>
          <summary>计算详情</summary>
          <p className="note">
            活动和等待回收的状态都占额度，同一双向会话计一次。其他用户已分配包含待生效和待释放容量。
          </p>
          {current && (
            <>
              <p className="note">
                系统保留 {integer(current.capacity.system_reserved_states)} 条，已从宿主总能力扣除。本用户仍持有{' '}
                {integer(current.held_states)} 条，本次额外分配 {integer(current.additional_states)} 条。
              </p>
              <p className="note">
                网络满载预算 = 固定开销 {capacity(current.capacity.fixed_memory_bytes)} + 宿主容量{' '}
                {integer(current.capacity.total_states)} × 每状态预算 {integer(current.capacity.bytes_per_state)} 字节 =
                {capacity(
                  current.capacity.fixed_memory_bytes +
                    current.capacity.total_states * current.capacity.bytes_per_state,
                )}
                ， 不超过网络预算 {capacity(current.capacity.memory_budget_bytes)}。这是预算上界，实际内存按需使用。
              </p>
              <p className="note">模型：{current.capacity.model}。Guest 已分配内存和宿主服务峰值单独保留。</p>
            </>
          )}
          {existing?.report?.applied && existing.fresh && existing.report.revision === existing.revision && (
            <p className="note">
              当前状态占用 {integer(existing.report.states)} 条。额度调小后，未释放容量继续保留，不会强制断开旧连接。
            </p>
          )}
        </details>
      </div>
      {preview.error && <ErrorBox error={preview.error} />}
    </section>
  );
}
