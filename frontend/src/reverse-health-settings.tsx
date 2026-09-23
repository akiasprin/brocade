import { useState } from 'react';
import {
  type ReverseHealthTuning,
  reverseHealthError,
  type ReverseHealthPolicy,
  type ReverseHealthOverride,
} from './api';
import { SettingsParameterSummary, type SettingsParameterMetric } from './ui/settings-parameter-summary';
import { SegmentedControl } from './ui/bits';

type BaseKey = Exclude<keyof ReverseHealthPolicy, 'tuning' | 'disconnect_on_health_failure'>;
type TuningKey = keyof ReverseHealthTuning;

/* 单位不写进字段名。`.setfld .unit` 就是字段行里放单位的位置，同段的「最大时钟偏差 …
   ms」「提前续期 … 天」用的都是它；写进名字里则每个标签都要多带一对括号，二十个字段
   排成一栏时，括号本身成了噪声。
   可访问名仍带单位全称：读屏念出的应当是「探活间隔（毫秒）」，而不是一个没有量纲的数。 */
const UNIT_NAME: Record<string, string> = { ms: '毫秒', '%': '百分比' };
const ariaOf = (name: string, unit?: string) => (unit ? `${name}（${UNIT_NAME[unit]}）` : name);
const U32_MAX = 4_294_967_295;

type Field =
  | { scope: 'base'; key: BaseKey; name: string; unit?: string; min: number; max: number }
  | { scope: 'tuning'; key: TuningKey; name: string; unit?: string; min: number; max: number };

/* 分组按语义，不按 `ReverseHealthPolicy` 与其 `tuning` 的嵌套。后者是接口形状——tuning
   是后加的一层结构，不是「常用 / 少用」的分界：探活间隔在外层、探活抖动在 tuning 里，
   两者是同一件事的两个参数，分置两处则读一条策略要来回对照。
 *
 * 每组末尾那一句原先集中在两段说明里，现在拆到它解释的那一组下面，句子本身未改。 */
const GROUPS: { name: string; note?: string; fields: Field[] }[] = [
  {
    name: '探活',
    note: '恢复应答次数必须在确认期限内完成。',
    fields: [
      { scope: 'base', key: 'probe_interval_ms', name: '探活间隔', unit: 'ms', min: 1, max: U32_MAX },
      { scope: 'base', key: 'probe_timeout_ms', name: '首次超时', unit: 'ms', min: 1, max: U32_MAX },
      { scope: 'base', key: 'confirm_timeout_ms', name: '确认期限', unit: 'ms', min: 1, max: U32_MAX },
      { scope: 'base', key: 'health_lease_ms', name: '健康证明有效期', unit: 'ms', min: 1, max: U32_MAX },
      { scope: 'tuning', key: 'probe_jitter_percent', name: '探活间隔抖动', unit: '%', min: 0, max: 100 },
      { scope: 'tuning', key: 'recovery_successes', name: '恢复所需连续应答次数', min: 1, max: U32_MAX },
    ],
  },
  {
    name: '容量',
    note: '健康上限约束可用池；排空中的旧隧道另行退出。',
    fields: [
      { scope: 'base', key: 'min_healthy_workers', name: '健康隧道下限', min: 1, max: U32_MAX },
      { scope: 'base', key: 'max_idle_ready_workers', name: '空闲隧道上限', min: 1, max: U32_MAX },
      { scope: 'tuning', key: 'max_healthy_workers', name: '健康隧道总数上限', min: 1, max: U32_MAX },
      { scope: 'tuning', key: 'spare_workers', name: '忙时备用隧道数', min: 0, max: U32_MAX },
      { scope: 'tuning', key: 'max_sessions_per_worker', name: '每条隧道业务并发上限', min: 0, max: 65535 },
      { scope: 'base', key: 'max_parallel_dials_per_pair', name: '每条链路并行建连上限', min: 1, max: U32_MAX },
    ],
  },
  {
    name: '建连与退避',
    note: '退避在起点至上限之间增长，每次带 50%–100% 抖动。',
    fields: [
      { scope: 'base', key: 'dial_ready_timeout_ms', name: '建连与验证总期限', unit: 'ms', min: 1, max: U32_MAX },
      { scope: 'tuning', key: 'reconnect_backoff_base_ms', name: '重试退避起点', unit: 'ms', min: 1, max: U32_MAX },
      { scope: 'base', key: 'reconnect_backoff_cap_ms', name: '重试退避上限', unit: 'ms', min: 1, max: U32_MAX },
      { scope: 'tuning', key: 'reconnect_stable_reset_ms', name: '稳定后重置退避', unit: 'ms', min: 0, max: U32_MAX },
    ],
  },
];

/* 摘要读的是当前值，不是又一句说明：折叠之后这一行要能替代下面二十个框回答「现在这条
   策略是什么」。空值和 NaN 显示为破折号——此时保存已被 `reversePoliciesError` 挡住，
   摘要不应把它显示成一个数。 */
const num = (v: number | null | undefined) => (v == null || Number.isNaN(v) ? '—' : String(v));

function summaryOf(policy: ReverseHealthPolicy): SettingsParameterMetric[] {
  const tuning = policy.tuning;
  return [
    { label: '探活 / 超时', value: `${num(policy.probe_interval_ms)} / ${num(policy.probe_timeout_ms)} ms` },
    { label: '健康隧道', value: `${num(policy.min_healthy_workers)}–${num(tuning.max_healthy_workers)} 条` },
    { label: '建连期限', value: `${num(policy.dial_ready_timeout_ms)} ms` },
    {
      label: '重试退避',
      value: `${num(tuning.reconnect_backoff_base_ms)}–${num(policy.reconnect_backoff_cap_ms)} ms`,
    },
  ];
}

function PolicyFields({
  value,
  editable,
  onChange,
}: {
  value: ReverseHealthPolicy;
  editable: boolean;
  onChange: (p: ReverseHealthPolicy) => void;
}) {
  const tuning = value.tuning;
  const read = (field: Field) => (field.scope === 'base' ? value[field.key] : tuning[field.key]);
  const write = (field: Field, next: number) =>
    field.scope === 'base'
      ? onChange({ ...value, [field.key]: next })
      : onChange({ ...value, tuning: { ...tuning, [field.key]: next } });

  return (
    <>
      <div className="settings-parameter-group">
        <p className="eyebrow">故障处理</p>
        <div className="settings-parameter-grid">
          <div className="setfld">
            <label>探活失败时已有请求</label>
            <div className="v">
              <SegmentedControl
                value={value.disconnect_on_health_failure}
                options={[
                  { value: false, label: '保留' },
                  { value: true, label: '主动断开' },
                ]}
                disabled={!editable}
                ariaLabel="探活失败时已有请求"
                onChange={disconnect_on_health_failure => onChange({ ...value, disconnect_on_health_failure })}
              />
            </div>
          </div>
        </div>
        <p className="hint settings-parameter-note">选择保留时停止分配新请求，已有请求自然结束后再回收隧道</p>
      </div>
      {GROUPS.map(group => (
        <div className="settings-parameter-group" key={group.name}>
          <p className="eyebrow">{group.name}</p>
          <div className="settings-parameter-grid">
            {group.fields.map(field => {
              const current = read(field);
              return (
                <div className="setfld" key={field.key}>
                  <label>{field.name}</label>
                  <div className="v">
                    <input
                      className="f"
                      aria-label={ariaOf(field.name, field.unit)}
                      type="number"
                      min={field.min}
                      max={field.max}
                      value={current == null || Number.isNaN(current) ? '' : current}
                      onChange={e => write(field, e.target.value === '' ? Number.NaN : Number(e.target.value))}
                    />
                    {field.unit && <span className="unit">{field.unit}</span>}
                  </div>
                </div>
              );
            })}
          </div>
          {group.note && <p className="hint settings-parameter-note">{group.note}</p>}
        </div>
      ))}
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
  editable,
  validationError,
  onPolicyChange,
  onOverridesChange,
}: {
  policy: ReverseHealthPolicy;
  rows: ReverseHealthOverride[];
  editable: boolean;
  validationError?: string | null;
  onPolicyChange: (p: ReverseHealthPolicy) => void;
  onOverridesChange: (rows: ReverseHealthOverride[]) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const toggle = () => setExpanded(open => !open);

  return (
    <div className="reverse-health-config" aria-label="反向隧道恢复设置">
      <SettingsParameterSummary
        label="反向隧道"
        metrics={summaryOf(policy)}
        expanded={expanded}
        editable={editable}
        controls="reverse-health-parameters"
        onToggle={toggle}
      />
      {validationError && (
        <p className="note settings-validation-error" role="alert">
          {validationError}
        </p>
      )}
      {expanded && (
        <>
          <div className="settings-parameter-content" id="reverse-health-parameters">
            <PolicyFields value={policy} editable={editable} onChange={onPolicyChange} />
            <p className="hint settings-parameter-note">
              所有在用和备用隧道持续双向探活。首次超时即停止分配新请求并补充连接；已有 TCP
              流不会自动重放。与连接策略一起保存后，需发布到链路两端。
            </p>
          </div>
          {rows.map((row, i) => (
            <fieldset className="reverse-health-override" key={i}>
              <legend>定向链路覆盖 {i + 1}</legend>
              <div className="settings-parameter-grid reverse-health-route">
                {(['chain', 'from', 'to'] as const).map((key, j) => (
                  <div className="setfld" key={key}>
                    <label>{['链路 ID', '流量起点节点 ID', '流量终点节点 ID'][j]}</label>
                    <div className="v">
                      <input
                        className="f reverse-health-route-id"
                        aria-label={['链路 ID', '流量起点节点 ID', '流量终点节点 ID'][j]}
                        value={row[key]}
                        onChange={e =>
                          onOverridesChange(rows.map((r, index) => (index === i ? { ...r, [key]: e.target.value } : r)))
                        }
                      />
                    </div>
                  </div>
                ))}
              </div>
              <PolicyFields
                value={row.health}
                editable={editable}
                onChange={health => onOverridesChange(rows.map((r, index) => (index === i ? { ...r, health } : r)))}
              />
              <button
                type="button"
                className="btn sm reverse-health-reset"
                onClick={() => onOverridesChange(rows.filter((_, index) => index !== i))}
              >
                恢复此链路的全局默认值
              </button>
            </fieldset>
          ))}
        </>
      )}
    </div>
  );
}
