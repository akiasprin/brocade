/* 机器自动限速：表单草稿与协议换算、执行状态，以及画在「流量统计」额度条上的旗标。
   限速与计量设置在同一张卡里编辑、共用一个保存（保存顺序见 TrafficAccountingCard），
   这里只负责限速自己的部分。触发与执行的语义见 docs/traffic.md。 */
import { useLayoutEffect, useRef, useState, type RefObject } from 'react';
import type {
  NodeTrafficLimitView,
  TrafficLimitLayer,
  TrafficLimitPhase,
  TrafficLimitPolicy,
  TrafficLimitRule,
  TrafficLimitThreshold,
} from '../api';
import { Ago, SegmentedControl, SegSwitch } from '../ui/bits';
import { useNow } from '../ui/clock';
import { bytes } from '../ui/format';
import { Icon } from '../ui/icons';
import { iso } from './telemetry-format';

const MAX_RULES = 8;
const GIB = 1024n ** 3n;
const TIB = 1024n ** 4n;
const U64_MAX = 18_446_744_073_709_551_615n;
const MIN_RATE_BPS = 1_000n;
const MAX_RATE_BPS = 1_000_000_000_000n;
/** 回执超过这个时长只算上次报告，不当作当前结果。 */
export const TRAFFIC_LIMIT_RECEIPT_FRESH_SECS = 300;

/* ══ 草稿 ══ */

/** 阈值单位：「%」是额度百分比，GiB/TiB 是本期绝对用量。类型由单位决定，不再单设一个类型下拉。 */
export type LimitThresholdUnit = '%' | 'GiB' | 'TiB';
type LimitRateUnit = 'Kbps' | 'Mbps';

export interface LimitDraftRule {
  amount: string;
  unit: LimitThresholdUnit;
  rate: string;
  rateUnit: LimitRateUnit;
  /** 载入时的绝对阈值：数值与单位都没改时原样回传，十进制显示的往返不改变字节数。 */
  original?: { amount: string; unit: LimitThresholdUnit; threshold: TrafficLimitThreshold };
}

export interface TrafficLimitForm {
  enabled: boolean;
  layer: TrafficLimitLayer;
  rules: LimitDraftRule[];
}

const LAYERS: ReadonlyArray<{ value: TrafficLimitLayer; label: string; note: string }> = [
  { value: 'application', label: '应用层', note: 'Xray 业务共用一个上限，适合受限容器；系统与其他程序不受限。' },
  { value: 'system', label: '网卡', note: '默认出口网卡收、发分别限速，全部连接共享；需要 tc、IFB 与网络管理权限。' },
  { value: 'both', label: '双层', note: '网卡与 Xray 同时限速，两层分别确认结果。' },
];

/** 字节数按单位写成最多六位小数（四舍五入，去掉末尾的 0）。 */
function decimalOf(value: bigint, unit: bigint): string {
  const scaled = (value * 1_000_000n + unit / 2n) / unit;
  const fraction = (scaled % 1_000_000n).toString().padStart(6, '0').replace(/0+$/, '');
  return fraction ? `${scaled / 1_000_000n}.${fraction}` : String(scaled / 1_000_000n);
}

/** Mbps 输入最多三位小数，只能表示 1000 bps 的整数倍；其余速度改用 Kbps，保证原样往返。 */
function rateUnitOf(rateBps: number): LimitRateUnit {
  return rateBps >= 1_000_000 && rateBps % 1_000 === 0 ? 'Mbps' : 'Kbps';
}

export function limitRateText(rateBps: number): string {
  const unit = rateUnitOf(rateBps);
  return `${rateBps / (unit === 'Mbps' ? 1_000_000 : 1_000)} ${unit}`;
}

function draftRule(rule: TrafficLimitRule): LimitDraftRule {
  const rateUnit = rateUnitOf(rule.rate_bps);
  const rate = String(rule.rate_bps / (rateUnit === 'Mbps' ? 1_000_000 : 1_000));
  if (rule.threshold.kind === 'quota_percent')
    return { amount: String(rule.threshold.basis_points / 100), unit: '%', rate, rateUnit };
  const value = BigInt(rule.threshold.bytes);
  const unit: LimitThresholdUnit = value >= TIB ? 'TiB' : 'GiB';
  const amount = decimalOf(value, unit === 'TiB' ? TIB : GIB);
  return { amount, unit, rate, rateUnit, original: { amount, unit, threshold: rule.threshold } };
}

/** 已保存策略的表单。规则按触发用量排序，与额度条上的顺序一致；编辑中新加的留在末尾，不跳行。 */
export function trafficLimitForm(policy: TrafficLimitPolicy | undefined, quota: string | null): TrafficLimitForm {
  if (!policy) return { enabled: false, layer: 'application', rules: [] };
  const quotaBytes = quota === null ? null : Number(quota);
  const at = (rule: TrafficLimitRule) =>
    rule.threshold.kind === 'absolute'
      ? Number(rule.threshold.bytes)
      : quotaBytes === null
        ? Infinity
        : (quotaBytes * rule.threshold.basis_points) / 10_000;
  const rules = [...policy.rules].sort((a, b) => at(a) - at(b) || a.rate_bps - b.rate_bps);
  return { enabled: policy.enabled, layer: policy.layer, rules: rules.map(draftRule) };
}

function exactDecimal(raw: string, decimals: number): { numerator: bigint; scale: bigint } | null {
  const value = raw.trim();
  if (!new RegExp(`^\\d+(?:\\.\\d{1,${decimals}})?$`).test(value)) return null;
  const [whole, fraction = ''] = value.split('.');
  const scale = 10n ** BigInt(fraction.length);
  return { numerator: BigInt(whole) * scale + BigInt(fraction || '0'), scale };
}

function limitThreshold(rule: LimitDraftRule): TrafficLimitThreshold | null {
  if (rule.unit === '%') {
    const parsed = exactDecimal(rule.amount, 2);
    if (!parsed) return null;
    const basisPoints = (parsed.numerator * 100n) / parsed.scale;
    return basisPoints > 0n && basisPoints <= 10_000n
      ? { kind: 'quota_percent', basis_points: Number(basisPoints) }
      : null;
  }
  if (rule.original && rule.original.amount === rule.amount && rule.original.unit === rule.unit)
    return rule.original.threshold;
  const parsed = exactDecimal(rule.amount, 6);
  if (!parsed) return null;
  const value = (parsed.numerator * (rule.unit === 'TiB' ? TIB : GIB) + parsed.scale / 2n) / parsed.scale;
  return value > 0n && value <= U64_MAX ? { kind: 'absolute', bytes: value.toString() } : null;
}

function limitRate(rule: LimitDraftRule): number | null {
  const parsed = exactDecimal(rule.rate, 3);
  if (!parsed) return null;
  const bits = parsed.numerator * (rule.rateUnit === 'Mbps' ? 1_000_000n : 1_000n);
  if (bits % parsed.scale !== 0n) return null;
  const rate = bits / parsed.scale;
  return rate >= MIN_RATE_BPS && rate <= MAX_RATE_BPS ? Number(rate) : null;
}

/** 表单转成协议策略；任一条无效、开启却没有规则、或开启时按百分比却没有额度，都返回 null。 */
export function trafficLimitPolicy(form: TrafficLimitForm, quota: string | null): TrafficLimitPolicy | null {
  if (form.rules.length > MAX_RULES || (form.enabled && form.rules.length === 0)) return null;
  const rules: TrafficLimitRule[] = [];
  for (const rule of form.rules) {
    const threshold = limitThreshold(rule);
    const rate = limitRate(rule);
    if (!threshold || rate === null) return null;
    if (threshold.kind === 'quota_percent' && form.enabled && !quota) return null;
    rules.push({ threshold, rate_bps: rate });
  }
  return { enabled: form.enabled, layer: form.layer, rules };
}

/** 无法保存的原因，一次只说一条。 */
export function trafficLimitProblem(form: TrafficLimitForm, quota: string | null): string | null {
  if (form.enabled && form.rules.length === 0) return '开启自动限速至少需要一条规则。';
  if (form.rules.some(rule => !limitThreshold(rule)))
    return '阈值需为大于 0 的数字：百分比不超过 100、最多两位小数，用量最多六位小数。';
  if (form.rules.some(rule => limitRate(rule) === null)) return '速度需在 1 Kbps 到 1 Tbps 之间，最多三位小数。';
  if (form.enabled && !quota && form.rules.some(rule => rule.unit === '%')) return '按百分比限速需要先填写流量额度。';
  return null;
}

/** 已保存的策略是否离不开额度：开启且含百分比规则。 */
export function trafficLimitNeedsQuota(policy: TrafficLimitPolicy | undefined): boolean {
  return !!policy?.enabled && policy.rules.some(rule => rule.threshold.kind === 'quota_percent');
}

/** 开启时补的第一条：有额度用 90%，没有额度用 1 TiB，都限至 100 Mbps。 */
function firstRule(quota: string | null): LimitDraftRule {
  return quota
    ? { amount: '90', unit: '%', rate: '100', rateUnit: 'Mbps' }
    : { amount: '1', unit: 'TiB', rate: '100', rateUnit: 'Mbps' };
}

/** 新规则接在最后一条之后：百分比加 5 个点，绝对用量加 1 TiB 或 512 GiB；速度减半。 */
function nextRule(rules: LimitDraftRule[], quota: string | null): LimitDraftRule {
  const last = rules[rules.length - 1];
  if (!last) return firstRule(quota);
  const half = Math.max(1_000, Math.round((limitRate(last) ?? 100_000_000) / 2));
  const rateUnit = rateUnitOf(half);
  const rate = String(half / (rateUnit === 'Mbps' ? 1_000_000 : 1_000));
  const amount = Number(last.amount);
  if (!Number.isFinite(amount)) return { ...firstRule(quota), rate, rateUnit };
  if (last.unit === '%') return { amount: String(Math.min(100, amount + 5)), unit: '%', rate, rateUnit };
  return { amount: String(amount + (last.unit === 'TiB' ? 1 : 512)), unit: last.unit, rate, rateUnit };
}

/* ══ 阶梯：规则按触发用量排开 ══ */

export interface LimitStep {
  /** 规则在表单中的序号。 */
  index: number;
  /** 触发用量（字节）。 */
  at: number;
  rate: number;
  /** 阈值的原写法：百分比或绝对用量。 */
  label: string;
  /** 越过时会让上限更低。此前已有更低限速的规则永远不起作用。 */
  effective: boolean;
  reached: boolean;
  /** 决定当前上限的一档。 */
  active: boolean;
}

/** 多条命中时取最低速度：沿用量走一遍，只有让速度继续下降的规则才是有效的一档。
 *  百分比规则在没有额度时无法落点，无效的规则也不画。 */
export function trafficLimitSteps(form: TrafficLimitForm, quota: number | null, used: number | null): LimitStep[] {
  const steps: LimitStep[] = [];
  form.rules.forEach((rule, index) => {
    const threshold = limitThreshold(rule);
    const rate = limitRate(rule);
    if (!threshold || rate === null) return;
    if (threshold.kind === 'quota_percent') {
      if (quota === null) return;
      steps.push({
        index,
        at: (quota * threshold.basis_points) / 10_000,
        rate,
        label: `${threshold.basis_points / 100}%`,
        effective: false,
        reached: false,
        active: false,
      });
    } else {
      const at = Number(threshold.bytes);
      steps.push({ index, at, rate, label: bytes(at), effective: false, reached: false, active: false });
    }
  });
  steps.sort((a, b) => a.at - b.at || a.rate - b.rate || a.index - b.index);
  let cap = Infinity;
  let active: LimitStep | null = null;
  for (const step of steps) {
    step.effective = step.rate < cap;
    if (step.effective) cap = step.rate;
    step.reached = used !== null && used >= step.at;
    if (step.effective && step.reached) active = step;
  }
  if (active) active.active = true;
  return steps;
}

function stepTitle(step: LimitStep): string {
  const threshold = step.label.endsWith('%') ? `${step.label}（${bytes(step.at)}）` : step.label;
  return `用量达到 ${threshold} 后限至 ${limitRateText(step.rate)}${step.effective ? '' : '；此前已有更低的限速，这条不会生效'}`;
}

/* ══ 执行状态：已保存的策略 + 机器回执 ══ */

type LimitTone = 'gold' | 'err' | 'mute';
const PHASE: Record<TrafficLimitPhase, string> = {
  disabled: '未启用',
  waiting: '等待应用确认',
  ready: '未达到阈值',
  limited: '已限速',
  error: '应用失败',
};

interface LimitLayerState {
  layer: 'system' | 'application';
  name: string;
  text: string;
  tone: LimitTone | null;
  rate: number | null;
  error: string | null;
}

export interface TrafficLimitStatus {
  text: string;
  tone: LimitTone | null;
  /** 标题栏状态片；平稳的状态（未启用、未达到阈值）不出现。 */
  chip: { text: string; tone: LimitTone } | null;
  confirmed: boolean;
  /** 机器在当前修订下确认的上限。 */
  rate: number | null;
  layers: LimitLayerState[];
  /** 没有落到某一层的错误。 */
  error: string | null;
  reportedAt: number | null;
}

/** 当前修订的回执在 300 秒内有效；过期或旧修订按等待处理。 */
export function trafficLimitStatus(limit: NodeTrafficLimitView | undefined, nowSecs: number): TrafficLimitStatus {
  if (!limit)
    return {
      text: '控制面待升级',
      tone: 'mute',
      chip: null,
      confirmed: false,
      rate: null,
      layers: [],
      error: null,
      reportedAt: null,
    };
  const { policy, observation } = limit;
  const reportedAt = limit.reported_at_unix_secs;
  const confirmed =
    !!observation &&
    observation.revision === limit.revision &&
    reportedAt !== null &&
    nowSecs - reportedAt < TRAFFIC_LIMIT_RECEIPT_FRESH_SECS;
  const layers: LimitLayerState[] = [];
  for (const layer of ['system', 'application'] as const) {
    const result = observation?.[layer];
    if (!result) continue;
    layers.push({
      layer,
      name: layer === 'system' ? '网卡' : 'Xray',
      text: confirmed ? PHASE[result.phase] : `上次报告 · ${PHASE[result.phase]}`,
      tone: !confirmed ? 'mute' : result.phase === 'limited' ? 'gold' : result.phase === 'error' ? 'err' : null,
      rate: result.rate_bps,
      error: result.error,
    });
  }
  const base = {
    confirmed,
    layers,
    reportedAt,
    rate: confirmed && observation?.phase === 'limited' ? (observation.rate_bps ?? null) : null,
    error: confirmed && !layers.some(layer => layer.error) ? (observation?.error ?? null) : null,
  };
  if (!limit.supported && policy.enabled)
    return { ...base, text: 'Agent 待升级', tone: 'mute', chip: { text: 'Agent 待升级', tone: 'mute' } };
  if (!confirmed) {
    if (policy.enabled)
      return { ...base, text: '等待应用确认', tone: 'mute', chip: { text: '限速待确认', tone: 'mute' } };
    if (observation?.rate_bps)
      return { ...base, text: '等待解除限速', tone: 'mute', chip: { text: '解除限速中', tone: 'mute' } };
    if (limit.revision > 0)
      return { ...base, text: '等待应用确认', tone: 'mute', chip: { text: '限速待确认', tone: 'mute' } };
    return { ...base, text: '未启用', tone: null, chip: null };
  }
  switch (observation!.phase) {
    case 'limited':
      return {
        ...base,
        text: '已限速',
        tone: 'gold',
        chip: { text: `限速 ${limitRateText(observation!.rate_bps ?? 0)}`, tone: 'gold' },
      };
    case 'error':
      return { ...base, text: '应用失败', tone: 'err', chip: { text: '限速应用失败', tone: 'err' } };
    case 'waiting':
      return { ...base, text: '等待应用确认', tone: 'mute', chip: { text: '限速待确认', tone: 'mute' } };
    default:
      return { ...base, text: PHASE[observation!.phase], tone: null, chip: null };
  }
}

/** 状态随回执过期而变化，用共享的一秒节拍重算；只让用到它的小块重绘。 */
function useTrafficLimitStatus(limit: NodeTrafficLimitView | undefined): TrafficLimitStatus {
  return trafficLimitStatus(limit, useNow() / 1000);
}

/** 标题栏的状态片：正在限速为金色，有一层失败为错误色，等待类为中性。 */
export function TrafficLimitChip({ limit }: { limit: NodeTrafficLimitView | undefined }) {
  const status = useTrafficLimitStatus(limit);
  if (!status.chip) return null;
  return (
    <span
      className={`node-traffic-limit-chip ${status.chip.tone}`}
      title={status.reportedAt === null ? undefined : `机器回执于 ${iso(status.reportedAt)}`}
    >
      {status.chip.tone !== 'mute' && <i aria-hidden="true" />}
      {status.chip.text}
    </span>
  );
}

/* ══ 额度条下的旗标 ══ */

function useElementWidth<T extends HTMLElement>(): [RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width];
}

/** 等宽字估宽：ASCII 约 0.6 em，其余按 1 em。 */
const textWidth = (text: string, size: number) =>
  [...text].reduce((sum, char) => sum + (char.charCodeAt(0) > 0x7f ? size : size * 0.6), 0);

interface FlagSlot {
  step: LimitStep;
  px: number;
  width: number;
  priority: number;
}
const FLAG_GAP = 6;

/** 旗面在旗杆右侧或左侧，取互不重叠且翻转最少的排法。怎么排都放不下时去掉最不重要的一面再试：
 *  当前一档最重要，其次是下一档，再按离当前用量的远近，永远不会生效的最后。去掉的只留旗杆。 */
function placeFlags(slots: FlagSlot[], width: number): Map<number, boolean> {
  const sorted = [...slots].sort((a, b) => a.px - b.px);
  const shown = new Set(sorted);
  while (shown.size > 0) {
    const row = sorted.filter(slot => shown.has(slot));
    const flips = fewestFlips(row, width);
    if (flips) return new Map(row.map((slot, index) => [slot.step.index, flips[index]]));
    shown.delete(row.reduce((worst, slot) => (slot.priority > worst.priority ? slot : worst)));
  }
  return new Map();
}

function fewestFlips(row: FlagSlot[], width: number): boolean[] | null {
  let best: boolean[] | null = null;
  let fewest = Infinity;
  for (let mask = 0; mask < 1 << row.length; mask++) {
    let end = -Infinity;
    let flips = 0;
    let fits = true;
    for (let index = 0; index < row.length && fits; index++) {
      const flip = ((mask >> index) & 1) === 1;
      const { px, width: size } = row[index];
      const from = flip ? px - size : px;
      fits = from >= 0 && from + size <= width && from >= end + FLAG_GAP;
      end = from + size;
      if (flip) flips += 1;
    }
    if (fits && flips < fewest) {
      fewest = flips;
      best = row.map((_, index) => ((mask >> index) & 1) === 1);
    }
  }
  return best;
}

/** 额度条下方：每档一根旗杆，旗面两行写阈值与速度。正在起作用的一档在机器确认限速后转金色。 */
export function TrafficLimitFlags({
  steps,
  position,
  used,
  limited,
}: {
  steps: LimitStep[];
  /** 字节数在条上的百分比位置，与额度条同一比例。 */
  position: (value: number) => number;
  used: number;
  limited: boolean;
}) {
  const [ref, width] = useElementWidth<HTMLSpanElement>();
  const next = steps.find(step => step.effective && !step.reached) ?? null;
  const flips = placeFlags(
    steps.map(step => ({
      step,
      px: (position(step.at) / 100) * width,
      width: Math.max(textWidth(step.label, 10), textWidth(limitRateText(step.rate), 10.5)) + 7,
      priority: step.active ? 0 : step === next ? 1 : step.effective ? 2 + Math.abs(step.at - used) : Infinity,
    })),
    width,
  );
  return (
    <span className="node-traffic-flags" ref={ref}>
      {steps.map(step => {
        const flip = flips.get(step.index);
        const className = [
          'node-traffic-flag',
          flip && 'flip',
          flip === undefined && 'bare',
          step.active && limited && 'on',
          !step.effective && 'idle',
        ]
          .filter(Boolean)
          .join(' ');
        return (
          <span
            key={step.index}
            className={className}
            style={{ left: `${position(step.at)}%` }}
            title={stepTitle(step)}
          >
            {flip !== undefined && (
              <>
                <b>{step.label}</b>
                {limitRateText(step.rate)}
              </>
            )}
          </span>
        );
      })}
    </span>
  );
}

/* ══ 设置行：自动限速 / 执行层 / 规则 ══ */

/** 自动限速的几行设置，接在计量设置的字段网格里。quota 是表单里当前的额度（字节字符串），
 *  百分比规则据此校验；steps 用来标出正在起作用和永远不会生效的规则。 */
export function TrafficLimitRows({
  limit,
  form,
  steps,
  quota,
  canEdit,
  saving,
  onChange,
}: {
  limit: NodeTrafficLimitView | undefined;
  form: TrafficLimitForm;
  steps: LimitStep[];
  quota: string | null;
  canEdit: boolean;
  saving: boolean;
  onChange: (form: TrafficLimitForm) => void;
}) {
  const locked = !canEdit || saving || !limit;
  return (
    <>
      <div className="row">
        <span className="k">自动限速</span>
        <span className="v">
          <span className="nd-ctl-line">
            <SegSwitch
              checked={form.enabled}
              disabled={locked}
              off="关闭"
              on="开启"
              ariaLabel="机器自动限速开关"
              onChange={enabled =>
                onChange({
                  ...form,
                  enabled,
                  rules: enabled && form.rules.length === 0 ? [firstRule(quota)] : form.rules,
                })
              }
            />
            <TrafficLimitState limit={limit} />
          </span>
        </span>
      </div>
      {limit && form.enabled && (
        <>
          <div className="row">
            <span className="k">执行层</span>
            <span className="v">
              <SegmentedControl
                value={form.layer}
                options={LAYERS.map(layer => ({ value: layer.value, label: layer.label }))}
                disabled={locked}
                ariaLabel="限速执行层"
                onChange={layer => onChange({ ...form, layer })}
              />
              <TrafficLimitLayerNote limit={limit} layer={form.layer} />
            </span>
          </div>
          <div className="row">
            <span className="k">规则</span>
            <span className="v">
              <TrafficLimitRules
                limit={limit}
                form={form}
                steps={steps}
                quota={quota}
                canEdit={canEdit}
                locked={locked}
                onChange={onChange}
              />
            </span>
          </div>
        </>
      )}
    </>
  );
}

/** 开关右侧：已保存策略的执行结果。回执时间放在状态区之外，免得读屏每秒播报。 */
function TrafficLimitState({ limit }: { limit: NodeTrafficLimitView | undefined }) {
  const status = useTrafficLimitStatus(limit);
  if (status.text === '未启用') return null;
  return (
    <span className={`node-traffic-limit-state${status.tone ? ` ${status.tone}` : ''}`}>
      <span role="status">
        {status.text}
        {status.text === '已限速' && status.rate !== null && ` ${limitRateText(status.rate)}`}
        {status.error && <em>：{status.error}</em>}
      </span>
      {status.confirmed && status.reportedAt !== null && (
        <small>
          <Ago at={iso(status.reportedAt)} />
        </small>
      )}
    </span>
  );
}

/** 执行层下面一行：默认写所选层的说明。两层分别回执、或有一层出错时改写各层结果——
 *  只有一层且正常时，结果与开关旁的状态是同一句话；过期回执也不写在这里。 */
function TrafficLimitLayerNote({ limit, layer }: { limit: NodeTrafficLimitView; layer: TrafficLimitLayer }) {
  const status = useTrafficLimitStatus(limit);
  const receipts =
    status.confirmed &&
    layer === limit.policy.layer &&
    (status.layers.length > 1 || status.layers.some(entry => entry.error !== null));
  if (!receipts) return <span className="sub">{LAYERS.find(entry => entry.value === layer)?.note}</span>;
  return (
    <span className="sub node-traffic-limit-layers" aria-label="限速执行结果">
      {status.layers.map(entry => (
        <span key={entry.layer} className={entry.tone ?? undefined}>
          {entry.name} {entry.text}
          {entry.rate !== null && ` ${limitRateText(entry.rate)}`}
          {entry.error && <em>：{entry.error}</em>}
        </span>
      ))}
    </span>
  );
}

const THRESHOLD_UNITS: ReadonlyArray<LimitThresholdUnit> = ['%', 'GiB', 'TiB'];
const RATE_UNITS: ReadonlyArray<LimitRateUnit> = ['Kbps', 'Mbps'];

/** 一行一条：「达到 [80 %] 限至 [100 Mbps]」，数值与单位拼成一格。行首一格圆点：机器确认限速时，
 *  正在起作用的一条为金色实心；此前已有更低限速、永远不会生效的一条为空心并整行变淡。 */
function TrafficLimitRules({
  limit,
  form,
  steps,
  quota,
  canEdit,
  locked,
  onChange,
}: {
  limit: NodeTrafficLimitView;
  form: TrafficLimitForm;
  steps: LimitStep[];
  quota: string | null;
  canEdit: boolean;
  locked: boolean;
  onChange: (form: TrafficLimitForm) => void;
}) {
  const limited = useTrafficLimitStatus(limit).rate !== null;
  const update = (index: number, patch: Partial<LimitDraftRule>) =>
    onChange({ ...form, rules: form.rules.map((rule, i) => (i === index ? { ...rule, ...patch } : rule)) });
  return (
    <div className="node-traffic-rules">
      {form.rules.map((rule, index) => {
        const step = steps.find(entry => entry.index === index);
        const amountInvalid = !limitThreshold(rule) || (form.enabled && rule.unit === '%' && !quota);
        const rateInvalid = limitRate(rule) === null;
        const className = ['node-traffic-rule', step?.active && limited && 'on', step && !step.effective && 'idle']
          .filter(Boolean)
          .join(' ');
        return (
          <div
            key={index}
            className={className}
            title={step && !step.effective ? '此前已有更低的限速，这条不会生效' : undefined}
          >
            <span className="node-traffic-rule-k">达到</span>
            <span className="node-traffic-num">
              <input
                className="f mono"
                inputMode="decimal"
                value={rule.amount}
                disabled={locked}
                aria-label={`规则 ${index + 1} 阈值`}
                aria-invalid={amountInvalid || undefined}
                onChange={event => update(index, { amount: event.target.value })}
              />
              <select
                className="f words"
                value={rule.unit}
                disabled={locked}
                aria-label={`规则 ${index + 1} 阈值单位`}
                onChange={event => update(index, { unit: event.target.value as LimitThresholdUnit })}
              >
                {THRESHOLD_UNITS.map(unit => (
                  <option key={unit} value={unit}>
                    {unit}
                  </option>
                ))}
              </select>
            </span>
            <span className="node-traffic-rule-k">限至</span>
            <span className="node-traffic-num">
              <input
                className="f mono"
                inputMode="decimal"
                value={rule.rate}
                disabled={locked}
                aria-label={`规则 ${index + 1} 限速`}
                aria-invalid={rateInvalid || undefined}
                onChange={event => update(index, { rate: event.target.value })}
              />
              <select
                className="f words"
                value={rule.rateUnit}
                disabled={locked}
                aria-label={`规则 ${index + 1} 速度单位`}
                onChange={event => update(index, { rateUnit: event.target.value as LimitRateUnit })}
              >
                {RATE_UNITS.map(unit => (
                  <option key={unit} value={unit}>
                    {unit}
                  </option>
                ))}
              </select>
            </span>
            {canEdit && (
              <button
                type="button"
                className="node-traffic-rule-remove"
                aria-label={`删除限速规则 ${index + 1}`}
                disabled={locked}
                onClick={() => onChange({ ...form, rules: form.rules.filter((_, i) => i !== index) })}
              >
                <Icon of="close" size={12} />
              </button>
            )}
          </div>
        );
      })}
      {canEdit && form.rules.length < MAX_RULES && (
        <button
          type="button"
          className="btn sm node-traffic-rule-add"
          disabled={locked}
          onClick={() => onChange({ ...form, rules: [...form.rules, nextRule(form.rules, quota)] })}
        >
          <Icon of="plus" size={12} />
          添加规则
        </button>
      )}
    </div>
  );
}
