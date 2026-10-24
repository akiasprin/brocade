// Shared parts of the 流量统计 + 自动限速 mockup: card state, settings rows, rule editor and the three runways.
import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
  type RefObject,
} from 'react';
import type {
  NodeTrafficAccountingMode,
  NodeTrafficCycleKind,
  NodeTrafficItem,
  TrafficLimitLayer,
  TrafficLimitRule,
} from '../../src/api';
import { SegmentedControl, SegSwitch } from '../../src/ui/bits';
import { Icon } from '../../src/ui/icons';
import { machineQuotaBytes, machineQuotaTiB } from '../../src/ui/machine-traffic-quota';
import {
  agoText,
  amountInvalid,
  countedDirections,
  draftOf,
  exact,
  LAYERS,
  MODES,
  NOW,
  nextStep,
  rateInvalid,
  rateText,
  ruleOf,
  statusOf,
  stepsOf,
  trim,
  untilText,
  volume,
  type DraftRule,
  type LimitStatus,
  type RateUnit,
  type Step,
  type ThresholdUnit,
} from './node-traffic-limit-mockup-data';

/* ══ 卡片状态：计量设置与限速策略同一份草稿、同一个保存 ══ */

export type Role = 'system' | 'readonly';
export type Preset = 'none' | 'rules' | 'enable' | 'invalid' | 'popover';

export interface AccountingForm {
  mode: NodeTrafficAccountingMode;
  cycle: NodeTrafficCycleKind;
  resetMonth: string;
  resetDay: string;
  quota: string;
}
export interface LimitForm {
  enabled: boolean;
  layer: TrafficLimitLayer;
  rules: DraftRule[];
}
interface CardForm {
  acc: AccountingForm;
  limit: LimitForm;
}

export interface Card {
  item: NodeTrafficItem;
  canEdit: boolean;
  form: CardForm;
  dirty: boolean;
  setAcc: (patch: Partial<AccountingForm>) => void;
  setLimit: (patch: Partial<LimitForm>) => void;
  setRule: (index: number, patch: Partial<DraftRule>) => void;
  /** 追加一条规则，返回它的序号。 */
  addRule: () => number;
  removeRule: (index: number) => void;
  revert: () => void;
  /** 表单里的额度：null 为不限，undefined 为无效输入。 */
  quota: number | null | undefined;
  used: number | null;
  /** 画在进度条上的台阶；关闭时为空。 */
  steps: Step[];
  /** 已保存的策略与机器回执。 */
  status: LimitStatus;
  /** 已保存策略在当前用量下的台阶，用于状态区的预测值。 */
  savedSteps: Step[];
  start: number;
  end: number;
  elapsed: number;
  calibrating: boolean;
  setCalibrating: (open: boolean) => void;
  invalid: { day: boolean; quota: boolean; amount: boolean[]; rate: boolean[] };
  problem: string | null;
}

const DEFAULT_RULE: DraftRule = { amount: '90', unit: '%', rate: '100', rateUnit: 'Mbps' };

/** 载入时按触发用量排序，列表与进度条上的顺序一致；编辑中新加的规则留在末尾，不跳行。 */
function byThreshold(rules: TrafficLimitRule[], quota: number | null): TrafficLimitRule[] {
  const at = (rule: TrafficLimitRule) =>
    rule.threshold.kind === 'absolute'
      ? Number(rule.threshold.bytes)
      : quota === null
        ? Infinity
        : (quota * rule.threshold.basis_points) / 10_000;
  return [...rules].sort((a, b) => at(a) - at(b) || a.rate_bps - b.rate_bps);
}

function baseForm(item: NodeTrafficItem): CardForm {
  const saved = item.limit?.policy ?? { enabled: false, layer: 'application' as const, rules: [] };
  const policy = { ...saved, rules: byThreshold(saved.rules, item.quota_bytes === null ? null : Number(item.quota_bytes)) };
  return {
    acc: {
      mode: item.accounting_mode,
      cycle: item.cycle_kind,
      resetMonth: String(item.reset_month ?? 1),
      resetDay: String(item.reset_day),
      quota: machineQuotaTiB(item.quota_bytes),
    },
    limit: { enabled: policy.enabled, layer: policy.layer, rules: policy.rules.map(draftOf) },
  };
}

function presetForm(base: CardForm, preset: Preset): CardForm {
  const withRule = base.limit.rules.length > 0 ? base.limit.rules : [DEFAULT_RULE];
  switch (preset) {
    case 'rules': {
      const [first, ...rest] = withRule;
      const moved =
        first.unit === '%'
          ? { ...first, amount: String(Math.max(5, Number(first.amount) - 10)) }
          : { ...first, amount: trim(Number(first.amount) * 0.8) };
      const quota = Number(base.acc.quota) || 2;
      const added: DraftRule = { amount: trim(quota * 0.9), unit: 'TiB', rate: '50', rateUnit: 'Mbps' };
      return { ...base, limit: { ...base.limit, enabled: true, rules: [moved, ...rest, added].slice(0, 8) } };
    }
    case 'enable':
      return { ...base, limit: { ...base.limit, enabled: true, rules: withRule } };
    case 'invalid':
      return { ...base, acc: { ...base.acc, quota: '' }, limit: { ...base.limit, enabled: true, rules: withRule } };
    default:
      return base;
  }
}

export function useCard(item: NodeTrafficItem, role: Role, preset: Preset): Card {
  const base = useMemo(() => baseForm(item), [item]);
  const [form, setForm] = useState<CardForm>(() => presetForm(base, preset));
  const [calibrating, setCalibrating] = useState(false);
  const canEdit = role === 'system';

  const quotaBytes = machineQuotaBytes(form.acc.quota);
  const quota = quotaBytes === undefined ? undefined : quotaBytes === null ? null : Number(quotaBytes);
  const tracked = item.tracking_started_at_unix_secs !== null || item.calibrated_at_unix_secs !== null;
  const used = tracked ? Number(item.total_bytes) : null;
  const steps = form.limit.enabled ? stepsOf(form.limit.rules.map(ruleOf), quota ?? null, used) : [];
  const saved = item.limit?.policy;
  const savedSteps = saved?.enabled
    ? stepsOf(saved.rules, item.quota_bytes === null ? null : Number(item.quota_bytes), used)
    : [];

  const day = Number(form.acc.resetDay);
  const invalid = {
    day: !Number.isInteger(day) || day < 1 || day > 31,
    quota: quota === undefined,
    amount: form.limit.rules.map(
      rule => amountInvalid(rule) || (form.limit.enabled && rule.unit === '%' && quota === null),
    ),
    rate: form.limit.rules.map(rateInvalid),
  };
  const problem = invalid.day
    ? '重置日需为 1–31。'
    : invalid.quota
      ? '流量额度需为大于 0 的数字，最多两位小数；留空为不限。'
      : form.limit.enabled && form.limit.rules.some(rule => rule.unit === '%') && quota === null
        ? '按百分比限速需要先填流量额度。'
        : form.limit.rules.some(amountInvalid)
          ? '阈值需大于 0，百分比不超过 100。'
          : invalid.rate.some(Boolean)
            ? '速度需在 1 Kbps 到 1 Tbps 之间。'
            : form.limit.enabled && form.limit.rules.length === 0
              ? '开启时至少需要一条规则。'
              : null;

  const start = item.period_start_unix_secs;
  const end = item.period_end_unix_secs;
  const setLimit = (patch: Partial<LimitForm>) => setForm(current => ({ ...current, limit: { ...current.limit, ...patch } }));
  return {
    item,
    canEdit,
    form,
    dirty: JSON.stringify(form) !== JSON.stringify(base),
    setAcc: patch => setForm(current => ({ ...current, acc: { ...current.acc, ...patch } })),
    setLimit,
    setRule: (index, patch) =>
      setForm(current => ({
        ...current,
        limit: {
          ...current.limit,
          rules: current.limit.rules.map((rule, i) => (i === index ? { ...rule, ...patch } : rule)),
        },
      })),
    addRule: () => {
      const index = form.limit.rules.length;
      setLimit({ rules: [...form.limit.rules, suggestRule(form.limit.rules, quota ?? null)] });
      return index;
    },
    removeRule: index => setLimit({ rules: form.limit.rules.filter((_, i) => i !== index) }),
    revert: () => {
      setForm(base);
      setCalibrating(false);
    },
    quota,
    used,
    steps,
    status: statusOf(item.limit, NOW),
    savedSteps,
    start,
    end,
    elapsed: Math.min(Math.max((NOW - start) / (end - start), 0), 1),
    calibrating,
    setCalibrating,
    invalid,
    problem,
  };
}

/** 新规则落在最后一档之后：百分比往上加 5 个点，绝对用量往上加 1 TiB；速度减半。 */
function suggestRule(rules: DraftRule[], quota: number | null): DraftRule {
  const last = rules[rules.length - 1];
  if (!last) return quota === null ? { amount: '1', unit: 'TiB', rate: '100', rateUnit: 'Mbps' } : DEFAULT_RULE;
  const rate = ruleOf(last)?.rate_bps ?? 100_000_000;
  const half = Math.max(1_000, Math.round(rate / 2));
  const rateUnit: RateUnit = half >= 1_000_000 ? 'Mbps' : 'Kbps';
  const rateValue = trim(half / (rateUnit === 'Mbps' ? 1_000_000 : 1_000));
  if (last.unit === '%')
    return { amount: String(Math.min(100, Number(last.amount) + 5)), unit: '%', rate: rateValue, rateUnit };
  return { amount: trim(Number(last.amount) + (last.unit === 'TiB' ? 1 : 512)), unit: last.unit, rate: rateValue, rateUnit };
}

/* ══ 布局工具 ══ */

export function useWidth<T extends HTMLElement>(): [RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    setWidth(element.getBoundingClientRect().width);
    const observer = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width];
}

/** 10.5px 等宽字：ASCII 约 6.3px，汉字约 10.5px。 */
export const textWidth = (text: string, size = 10.5) =>
  [...text].reduce((sum, char) => sum + (char.charCodeAt(0) > 0x7f ? size : size * 0.6), 0);

export interface Placed<T> {
  item: T;
  px: number;
  lane: number;
  flip: boolean;
}

/** 单行排布：每个标签可以排在落点右侧或左侧（flip），在互不重叠的排法里取翻转最少的一种；
 *  怎么排都放不下时，去掉最不重要的一个（priority 最大）再试。被去掉的 lane = -1，只留竖线。 */
export function placeRow<T>(
  labels: { item: T; px: number; w: number; priority: number }[],
  width: number,
  gap = 6,
): Placed<T>[] {
  const sorted = [...labels].sort((a, b) => a.px - b.px);
  const shown = new Set(sorted);
  while (shown.size > 0) {
    const row = sorted.filter(label => shown.has(label));
    const sides = bestSides(row, width, gap);
    if (sides)
      return sorted.map(label => {
        const index = row.indexOf(label);
        return { item: label.item, px: label.px, lane: index < 0 ? -1 : 0, flip: index >= 0 && sides[index] };
      });
    shown.delete(row.reduce((worst, label) => (label.priority > worst.priority ? label : worst)));
  }
  return sorted.map(label => ({ item: label.item, px: label.px, lane: -1, flip: false }));
}

function bestSides(row: { px: number; w: number }[], width: number, gap: number): boolean[] | null {
  if (row.length > 10) return null;
  let best: boolean[] | null = null;
  let fewest = Infinity;
  for (let mask = 0; mask < 1 << row.length; mask++) {
    let end = -Infinity;
    let flips = 0;
    let fits = true;
    for (let index = 0; index < row.length && fits; index++) {
      const flip = ((mask >> index) & 1) === 1;
      const { px, w } = row[index];
      const [from, to] = flip ? [px - w, px] : [px, px + w];
      fits = from >= 0 && to <= width && from >= end + gap;
      end = to;
      flips += flip ? 1 : 0;
    }
    if (fits && flips < fewest) {
      fewest = flips;
      best = row.map((_, index) => ((mask >> index) & 1) === 1);
    }
  }
  return best;
}

/** 当前那一档最重要，其次是下一档，再按离当前用量的远近；永远不会生效的规则排最后。 */
export function stepPriority(step: Step, steps: Step[], used: number | null): number {
  if (step.active) return 0;
  if (step === nextStep(steps)) return 1;
  if (!step.effective) return 1e12;
  return 2 + Math.abs(step.at - (used ?? 0));
}

/** 多道排布：标签从落点向右排；排不下就换到下一条道，靠右边缘时改为向左排。都放不下的只留竖线（lane = -1）。 */
export function placeLabels<T>(
  items: { item: T; px: number; w: number }[],
  width: number,
  lanes: number,
  gap = 8,
): Placed<T>[] {
  const ends: number[] = [];
  return items.map(({ item, px, w }) => {
    const flip = px + w > width && px - w >= 0;
    const [from, to] = flip ? [px - w, px] : [px, px + w];
    for (let lane = 0; lane < lanes; lane++) {
      if (ends[lane] === undefined || from >= ends[lane] + gap) {
        ends[lane] = to;
        return { item, px, lane, flip };
      }
    }
    return { item, px, lane: -1, flip };
  });
}

/** 进度条比例：有额度时满格 = 额度、已用与最高阈值中较大者；没有额度时按最高阈值再留出一段。 */
export function scaleOf(card: Card) {
  const quota = card.quota ?? null;
  const used = card.used ?? 0;
  const top = card.steps.reduce((max, step) => Math.max(max, step.at), 0);
  const full = quota !== null ? Math.max(quota, used, top) : Math.max(used, top) * 1.14;
  const x = (value: number) => (full > 0 ? (Math.min(Math.max(value, 0), full) / full) * 100 : 0);
  return { quota, used, full, x, over: quota !== null && used > quota, drawable: full > 0 && (quota !== null || card.steps.length > 0) };
}

export const stepTitle = (step: Step) =>
  `用量达到 ${step.label.endsWith('%') ? `${step.label}（${volume(step.at)}）` : step.label} 后限至 ${rateText(step.rate)}` +
  (step.effective ? '' : '；此前已有更低的限速，这条不会生效');

function runwayLabel(card: Card): string {
  const parts = [
    card.used === null ? '本期用量未上报' : `本期已用 ${exact(card.used)}`,
    card.quota ? `额度 ${volume(card.quota)}` : '未设额度',
    ...card.steps.filter(step => step.effective).map(step => `${step.label} 起限 ${rateText(step.rate)}`),
  ];
  return parts.join('，');
}

/* ══ 读数 ══ */

export function StatusChip({ status }: { status: LimitStatus }) {
  if (!status.chip) return null;
  return (
    <span
      className={`ntl-chip ${status.chip.tone}`}
      title={status.reportedAgo === null ? undefined : `机器回执于 ${agoText(status.reportedAgo)}`}
    >
      {status.chip.tone !== 'mute' && <i aria-hidden="true" />}
      {status.chip.text}
    </span>
  );
}

export function Source({ item }: { item: NodeTrafficItem }) {
  return (
    <span className="node-traffic-source">
      <span className="mono">{item.interface ?? '待上报'}</span>
    </span>
  );
}

/** 本期一行：与生产卡同一结构。 */
export function ReadingLine({ card }: { card: Card }) {
  const { used, end } = card;
  const quota = card.quota ?? null;
  return (
    <div className="node-traffic-line">
      <span className="node-traffic-used">
        本期
        <b>{used === null ? '—' : exact(used)}</b>
        {quota !== null && <span>/ {machineQuotaTiB(String(quota))} TiB</span>}
        {quota !== null && used !== null && (
          <span className={used > quota ? 'over' : undefined}>{Math.round((used / quota) * 100)}%</span>
        )}
      </span>
      <span className="node-traffic-reset">
        {untilText(end - NOW)}后重置 · {new Date(end * 1000).toISOString().slice(5, 10)} UTC
      </span>
    </div>
  );
}

export function Directions({ item }: { item: NodeTrafficItem }) {
  if (item.tracking_started_at_unix_secs === null) return null;
  const counted = countedDirections(item);
  return (
    <dl className="node-traffic-directions" aria-label="接收与发送流量">
      <div className={counted.rx ? 'rx' : 'rx off'}>
        <dt>
          <i aria-hidden="true" />
          接收
        </dt>
        <dd>{exact(Number(item.rx_bytes))}</dd>
      </div>
      <div className={counted.tx ? 'tx' : 'tx off'}>
        <dt>
          <i aria-hidden="true" />
          发送
        </dt>
        <dd>{exact(Number(item.tx_bytes))}</dd>
      </div>
    </dl>
  );
}

/* ══ 进度条 ①：切口 + 旗标（方案 A、C） ══ */

export function TickRunway({ card }: { card: Card }) {
  const [ref, width] = useWidth<HTMLSpanElement>();
  const { x, drawable } = scaleOf(card);
  if (!drawable) return null;
  /* 旗面两行：上面阈值、下面速度。两行比一行「80% 100 Mbps」窄一半，相邻两档贴着右端时也排得进一行。 */
  const flags = placeRow(
    card.steps.map(step => ({
      item: step,
      px: (x(step.at) / 100) * width,
      w: Math.max(textWidth(step.label, 10), textWidth(rateText(step.rate))) + 7,
      priority: stepPriority(step, card.steps, card.used),
    })),
    width,
  );
  return (
    <span className="ntl-run" ref={ref}>
      <BarCore card={card} />
      {card.steps.length > 0 && (
        <span className="ntl-flags" aria-hidden="true">
          {flags.map(({ item: step, lane, flip }) => (
            <span
              key={step.index}
              className={[
                'ntl-flag',
                flip && 'flip',
                lane < 0 && 'bare',
                step.active && card.status.rate !== null && 'on',
                !step.effective && 'idle',
              ]
                .filter(Boolean)
                .join(' ')}
              style={{ left: `${x(step.at)}%` }}
              title={stepTitle(step)}
            >
              {lane >= 0 && (
                <>
                  <b>{step.label}</b>
                  {rateText(step.rate)}
                </>
              )}
            </span>
          ))}
        </span>
      )}
    </span>
  );
}

/** 跑道本体：已用、超额、越过阈值后的斜纹、切口、时间进度或额度竖线。 */
export function BarCore({ card, cuts = true }: { card: Card; cuts?: boolean }) {
  const { quota, used, x, over } = scaleOf(card);
  const tracked = card.used !== null;
  const limitedFrom = card.steps.find(step => step.effective && step.reached);
  return (
    <span className="ntl-bar" role="img" aria-label={runwayLabel(card)}>
      <span className="cap-bar">
        {tracked && <i className="u" style={{ left: 0, width: `${x(over ? quota! : used)}%` }} />}
        {tracked && over && <i className="u x" style={{ left: `${x(quota!)}%`, width: `${x(used) - x(quota!)}%` }} />}
        {tracked && limitedFrom && (
          <i
            className="ntl-hatch"
            style={{ left: `${x(limitedFrom.at)}%`, width: `${x(used) - x(limitedFrom.at)}%` }}
          />
        )}
      </span>
      {cuts &&
        card.steps.map(step => (
          <i
            key={step.index}
            className={`ntl-cut${step.effective ? '' : ' idle'}`}
            style={{ left: `${x(step.at)}%` }}
          />
        ))}
      {over ? (
        <i className="cap-q" style={{ left: `${x(quota!)}%` }} />
      ) : (
        quota !== null &&
        tracked && (
          <i
            className="node-traffic-pace"
            style={{ left: `${x(quota * card.elapsed)}%` }}
            title={`本期时间已过 ${Math.round(card.elapsed * 100)}%`}
          />
        )
      )}
    </span>
  );
}

/* ══ 进度条 ②：限速天际线（方案 B） ══ */

/** 跑道上方加一条天际线：横轴仍是本期用量，线的高度是允许的速度（对数刻度；第一档之前不限速、最高）。
 *  已用多少交给下面的跑道；线上只有一个圆点标出当前位置，正在限速时与那一档的速度一起转金色。 */
export function Staircase({ card }: { card: Card }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const scale = scaleOf(card);
  const effective = card.steps.filter(step => step.effective);
  if (!scale.drawable) return null;
  if (effective.length === 0)
    return (
      <span className="ntl-run">
        <BarCore card={card} />
      </span>
    );
  const { quota, used, x, over } = scale;
  const tracked = card.used !== null;
  const rates = effective.map(step => step.rate);
  const hi = Math.log(Math.max(...rates));
  const lo = Math.log(Math.min(...rates));
  const level = (rate: number) => (hi === lo ? 0.36 : 0.14 + (0.4 * (Math.log(rate) - lo)) / (hi - lo));
  const zones = [
    { from: 0, to: effective[0].at, level: 0.74, step: null as Step | null },
    ...effective.map((step, index) => ({
      from: step.at,
      to: effective[index + 1]?.at ?? scale.full,
      level: level(step.rate),
      step,
    })),
  ];
  const X = (value: number) => x(value) * 10;
  const Y = (lv: number) => 100 - lv * 100;
  const profile = zones
    .map((zone, index) => `${index === 0 ? 'M0 ' : 'V'}${Y(zone.level)}H${X(zone.to)}`)
    .join('');
  const current = [...zones].reverse().find(zone => used >= zone.from) ?? zones[0];
  const limited = card.status.rate !== null && current.step !== null;
  /* 额度不进刻度：读数行已经写了「/ 2 TiB」，刻度只留阈值。 */
  const axis = placeRow(
    effective.map(step => ({
      item: step,
      px: (x(step.at) / 100) * width,
      w: textWidth(step.label, 10) + 5,
      priority: stepPriority(step, card.steps, card.used),
    })),
    width,
  );
  return (
    <div className="ntl-sky" ref={ref} role="img" aria-label={runwayLabel(card)}>
      <div className="ntl-sky-plot">
        <svg viewBox="0 0 1000 100" preserveAspectRatio="none" aria-hidden="true">
          {zones.map(
            (zone, index) =>
              zone.to > zone.from && (
                <rect
                  key={index}
                  className="wash"
                  x={X(zone.from)}
                  y={Y(zone.level)}
                  width={X(zone.to) - X(zone.from)}
                  height={zone.level * 100}
                />
              ),
          )}
          <path className="line" d={profile} vectorEffect="non-scaling-stroke" />
          {over && (
            <line className="quota" x1={X(quota!)} x2={X(quota!)} y1={0} y2={100} vectorEffect="non-scaling-stroke" />
          )}
        </svg>
        {skyLabels(zones, current, width, tracked ? (x(used) / 100) * width : null).map(
          ({ zone, flip, text }) => (
            <span
              key={zone.from}
              className={`ntl-sky-rate${flip ? ' flip' : ''}${zone === current ? ' now' : ''}${zone === current && limited ? ' on' : ''}`}
              style={{ left: `${X(zone.from) / 10}%`, bottom: `${zone.level * 100}%` }}
              title={zone.step ? stepTitle(zone.step) : '第一档之前不限速'}
            >
              {text}
            </span>
          ),
        )}
        {tracked && (
          <i
            className={`ntl-sky-dot${limited ? ' on' : ''}`}
            style={{ left: `clamp(5px, ${x(used)}%, calc(100% - 5px))`, bottom: `${current.level * 100}%` }}
            aria-hidden="true"
          />
        )}
      </div>
      <BarCore card={card} />
      <div className="ntl-sky-axis" aria-hidden="true">
        {axis.map(
          ({ item: step, lane, flip }) =>
            lane >= 0 && (
              <span key={step.index} className={flip ? 'flip' : undefined} style={{ left: `${x(step.at)}%` }}>
                {step.label}
              </span>
            ),
        )}
      </div>
    </div>
  );
}

interface SkyZone {
  from: number;
  to: number;
  level: number;
  step: Step | null;
}
const SKY_HEIGHT = 50;

/** 天际线上的速度标签：写在每一级线的上方、靠这一级的左端。当前一级必写；其余的要放得进自己那一段，
 *  最后一段放不下时改写在落差左侧、与这一级同高（字落在前一级的线下面，不压落差竖线）。
 *  与已写的标签或当前位置的圆点相碰就不写。 */
function skyLabels(zones: SkyZone[], current: SkyZone, width: number, dotPx: number | null) {
  const full = zones[zones.length - 1].to;
  const px = (value: number) => (value / full) * width;
  type Box = [number, number, number, number];
  const hits = (a: Box, b: Box) => a[0] < b[1] + 3 && b[0] < a[1] + 3 && a[2] < b[3] && b[2] < a[3];
  const taken: Box[] = [];
  if (dotPx !== null) {
    const y = current.level * SKY_HEIGHT;
    taken.push([dotPx - 6, dotPx + 6, y - 5.5, y + 5.5]);
  }
  const order = [current, ...zones.filter(zone => zone !== current)];
  const shown: { zone: SkyZone; flip: boolean; text: string }[] = [];
  for (const zone of order) {
    const text = zone.step ? rateText(zone.step.rate) : '不限速';
    const w = textWidth(text);
    const fits = px(zone.to) - px(zone.from) >= w + 10;
    const index = zones.indexOf(zone);
    const last = index === zones.length - 1;
    if (!fits && !last && zone !== current) continue;
    /* 当前一级放不下时只要不越出右端就照写；越出右端才翻到落差左侧。 */
    const flip = zone === current ? index > 0 && px(zone.from) + 5 + w > width : !fits && last;
    const x0 = flip ? px(zone.from) - 5 - w : px(zone.from) + 5;
    if (flip && zone !== current && x0 < px(zones[index - 1].from) + 3) continue;
    const y0 = zone.level * SKY_HEIGHT + 2;
    const box: Box = [x0, x0 + w, y0, y0 + 12];
    if (zone !== current && taken.some(other => hits(box, other))) continue;
    taken.push(box);
    shown.push({ zone, flip, text });
  }
  return shown;
}

/* ══ 进度条 ③：标签即规则（方案 D） ══ */

export function PinRunway({
  card,
  open,
  setOpen,
}: {
  card: Card;
  open: number | null;
  setOpen: (index: number | null) => void;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const scale = scaleOf(card);
  const { x } = scale;
  const showAdd = card.canEdit && card.form.limit.enabled && card.form.limit.rules.length < 8;
  if (!scale.drawable && !showAdd) return null;
  /* 每条规则都要点得到：先试一行（落点左右两侧都可以），排不下才分道。 */
  const pinLabels = card.steps.map(step => ({
    item: step,
    px: (x(step.at) / 100) * width,
    w: textWidth(rateText(step.rate)) + 14,
    priority: 0,
  }));
  const row = placeRow(pinLabels, width, 4);
  const pins = row.every(pin => pin.lane === 0) ? row : placeLabels(pinLabels, width, 3, 4);
  const lanes = Math.max(1, ...pins.map(pin => pin.lane + 1));
  const pinsHeight = card.steps.length > 0 ? lanes * 22 + 7 : 0;
  const ticks = placeRow(
    card.steps.map(step => ({
      item: step,
      px: (x(step.at) / 100) * width,
      w: textWidth(step.label, 10) + 5,
      priority: stepPriority(step, card.steps, card.used),
    })),
    width,
  );
  const openStep = card.steps.find(step => step.index === open) ?? null;
  return (
    <div className={`ntl-pinrun${showAdd ? ' with-add' : ''}`}>
      <div className="ntl-pinrun-track" ref={ref}>
        {card.steps.length > 0 && (
          <div className="ntl-pins" style={{ height: `${pinsHeight}px` }}>
            {pins.map(({ item: step, lane, flip }) => {
              const className = [
                'ntl-pin',
                flip && 'flip',
                step.active && 'on',
                !step.effective && 'idle',
                open === step.index && 'open',
              ]
                .filter(Boolean)
                .join(' ');
              const style = { left: `${x(step.at)}%`, '--lane': Math.max(lane, 0) } as CSSProperties;
              const text = rateText(step.rate);
              return card.canEdit ? (
                <span key={step.index} className={className} style={style}>
                  <button
                    type="button"
                    aria-label={`编辑规则：${stepTitle(step)}`}
                    aria-expanded={open === step.index}
                    onClick={() => setOpen(open === step.index ? null : step.index)}
                  >
                    {text}
                  </button>
                </span>
              ) : (
                <span key={step.index} className={className} style={style} title={stepTitle(step)}>
                  <span>{text}</span>
                </span>
              );
            })}
          </div>
        )}
        {scale.drawable ? <BarCore card={card} /> : <span className="ntl-bar empty" />}
        {card.steps.length > 0 && (
          <div className="ntl-ticks" aria-hidden="true">
            {ticks.map(
              ({ item: step, lane, flip }) =>
                lane >= 0 && (
                  <span
                    key={step.index}
                    className={`${flip ? 'flip' : ''}${step.effective ? '' : ' idle'}`}
                    style={{ left: `${x(step.at)}%` }}
                  >
                    {step.label}
                  </span>
                ),
            )}
          </div>
        )}
        {open !== null && card.form.limit.rules[open] && (
          <RulePopover
            card={card}
            index={open}
            anchor={openStep ? x(openStep.at) : 100}
            onClose={() => setOpen(null)}
          />
        )}
      </div>
      {showAdd && (
        <button
          type="button"
          className="ntl-pin-add"
          style={{ top: `${pinsHeight - 7}px` }}
          aria-label="添加限速规则"
          title="添加限速规则"
          onClick={() => setOpen(card.addRule())}
        >
          <Icon of="plus" size={12} />
        </button>
      )}
    </div>
  );
}

function RulePopover({
  card,
  index,
  anchor,
  onClose,
}: {
  card: Card;
  index: number;
  anchor: number;
  onClose: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  const rule = card.form.limit.rules[index];
  useEffect(() => {
    box.current?.querySelector('input')?.focus();
  }, [index]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => event.key === 'Escape' && close.current();
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  const parsed = ruleOf(rule);
  const quota = card.quota ?? null;
  const at =
    parsed && parsed.threshold.kind === 'quota_percent' && quota !== null
      ? (quota * parsed.threshold.basis_points) / 10_000
      : null;
  return (
    <div
      ref={box}
      className="ntl-pop"
      role="dialog"
      aria-label={`限速规则 ${index + 1}`}
      style={{ '--anchor': `${anchor}%` } as CSSProperties}
    >
      <div className="ntl-pop-row">
        <span className="k">达到</span>
        <AmountField card={card} index={index} />
        {at !== null && <span className="ntl-pop-eq">= {volume(at)}</span>}
      </div>
      <div className="ntl-pop-row">
        <span className="k">限至</span>
        <RateField card={card} index={index} />
      </div>
      <div className="ntl-pop-foot">
        <button
          type="button"
          className="btn sm"
          onClick={() => {
            card.removeRule(index);
            onClose();
          }}
        >
          删除
        </button>
        <span className="sp" />
        <button type="button" className="btn sm" onClick={onClose}>
          完成
        </button>
      </div>
    </div>
  );
}

/* ══ 设置 ══ */

/** 机器配置页的一行：标签 + 值。与 nodes.tsx 的 Row 同一结构。 */
export function Row({ k, children, className }: { k: ReactNode; children: ReactNode; className?: string }) {
  return (
    <div className={`row${className ? ` ${className}` : ''}`}>
      <span className="k">{k}</span>
      <span className="v">{children}</span>
    </div>
  );
}

export function AccountingRows({ card }: { card: Card }) {
  const { form, canEdit, invalid, item } = card;
  const acc = form.acc;
  return (
    <>
      <Row k="计量口径">
        <select
          className="f words ntl-mode"
          value={acc.mode}
          disabled={!canEdit}
          aria-label="机器流量计量口径"
          onChange={event => card.setAcc({ mode: event.target.value as NodeTrafficAccountingMode })}
        >
          {MODES.map(mode => (
            <option key={mode.value} value={mode.value}>
              {mode.label}
            </option>
          ))}
        </select>
        <span className="sub">{MODES.find(mode => mode.value === acc.mode)?.note}</span>
      </Row>
      <Row k="重置">
        <span className="nd-ctl-line">
          <SegmentedControl
            value={acc.cycle}
            options={[
              { value: 'monthly' as const, label: '每月' },
              { value: 'yearly' as const, label: '每年' },
            ]}
            disabled={!canEdit}
            ariaLabel="流量重置周期"
            onChange={cycle => card.setAcc({ cycle })}
          />
          {acc.cycle === 'yearly' && (
            <select
              className="f words ntl-month"
              value={acc.resetMonth}
              disabled={!canEdit}
              aria-label="重置月份"
              onChange={event => card.setAcc({ resetMonth: event.target.value })}
            >
              {Array.from({ length: 12 }, (_, index) => index + 1).map(month => (
                <option value={month} key={month}>
                  {month} 月
                </option>
              ))}
            </select>
          )}
          <input
            className="f mono ntl-day"
            type="number"
            min={1}
            max={31}
            value={acc.resetDay}
            disabled={!canEdit}
            aria-label="重置日"
            aria-invalid={invalid.day || undefined}
            onChange={event => card.setAcc({ resetDay: event.target.value })}
          />
          <span>日</span>
        </span>
      </Row>
      <Row k="流量额度">
        <span className="nd-ctl-line">
          <input
            className="f mono ntl-quota"
            inputMode="decimal"
            placeholder="不限"
            value={acc.quota}
            disabled={!canEdit}
            aria-label="机器流量额度"
            aria-invalid={invalid.quota || undefined}
            onChange={event => card.setAcc({ quota: event.target.value })}
          />
          <span>TiB</span>
        </span>
      </Row>
      <Row k="校准">
        {card.calibrating ? (
          <span className="nd-ctl-line">
            <input
              className="f mono ntl-quota"
              inputMode="decimal"
              placeholder="服务商读数"
              autoFocus
              aria-label="当前流量校准值"
            />
            <SegmentedControl
              value={'TiB' as const}
              options={[
                { value: 'GiB' as const, label: 'GiB' },
                { value: 'TiB' as const, label: 'TiB' },
              ]}
              ariaLabel="校准单位"
              onChange={() => undefined}
            />
            <button type="button" className="btn" onClick={() => card.setCalibrating(false)}>
              取消
            </button>
          </span>
        ) : (
          <span className="nd-ctl-line">
            <span>{item.calibrated_at_unix_secs === null ? '本期未校准' : '已校准'}</span>
            {canEdit && (
              <button type="button" className="btn" onClick={() => card.setCalibrating(true)}>
                校准
              </button>
            )}
          </span>
        )}
      </Row>
    </>
  );
}

const THRESHOLD_UNITS: { value: ThresholdUnit; label: string }[] = [
  { value: '%', label: '%' },
  { value: 'GiB', label: 'GiB' },
  { value: 'TiB', label: 'TiB' },
];
const RATE_UNITS: { value: RateUnit; label: string }[] = [
  { value: 'Kbps', label: 'Kbps' },
  { value: 'Mbps', label: 'Mbps' },
];

/** 数值与单位合成一格：输入框右侧接一个单位下拉。阈值的「%」就是额度百分比，不再单设类型下拉。 */
export function AmountField({ card, index }: { card: Card; index: number }) {
  const rule = card.form.limit.rules[index];
  return (
    <span className={`ntl-num${card.invalid.amount[index] ? ' bad' : ''}`}>
      <input
        className="f mono"
        inputMode="decimal"
        value={rule.amount}
        disabled={!card.canEdit}
        aria-label={`规则 ${index + 1} 阈值`}
        aria-invalid={card.invalid.amount[index] || undefined}
        onChange={event => card.setRule(index, { amount: event.target.value })}
      />
      <select
        className="f words"
        value={rule.unit}
        disabled={!card.canEdit}
        aria-label={`规则 ${index + 1} 阈值单位`}
        onChange={event => card.setRule(index, { unit: event.target.value as ThresholdUnit })}
      >
        {THRESHOLD_UNITS.map(unit => (
          <option key={unit.value} value={unit.value}>
            {unit.label}
          </option>
        ))}
      </select>
    </span>
  );
}

export function RateField({ card, index }: { card: Card; index: number }) {
  const rule = card.form.limit.rules[index];
  return (
    <span className={`ntl-num${card.invalid.rate[index] ? ' bad' : ''}`}>
      <input
        className="f mono"
        inputMode="decimal"
        value={rule.rate}
        disabled={!card.canEdit}
        aria-label={`规则 ${index + 1} 限速`}
        aria-invalid={card.invalid.rate[index] || undefined}
        onChange={event => card.setRule(index, { rate: event.target.value })}
      />
      <select
        className="f words"
        value={rule.rateUnit}
        disabled={!card.canEdit}
        aria-label={`规则 ${index + 1} 速度单位`}
        onChange={event => card.setRule(index, { rateUnit: event.target.value as RateUnit })}
      >
        {RATE_UNITS.map(unit => (
          <option key={unit.value} value={unit.value}>
            {unit.label}
          </option>
        ))}
      </select>
    </span>
  );
}

/** 规则列表：一行一条，「达到 [80 %] 限至 [100 Mbps]」。行首一格圆点：当前起作用的一条为金色实心，
 *  永远不会生效的一条（此前已有更低限速）为空心并整行变淡，原因写在悬停提示里。 */
export function RuleList({ card }: { card: Card }) {
  const { form, canEdit } = card;
  const stepOf = (index: number) => card.steps.find(step => step.index === index) ?? null;
  return (
    <div className="ntl-rules">
      {form.limit.rules.map((_, index) => {
        const step = stepOf(index);
        return (
          <div
            key={index}
            className={`ntl-rule${step?.active && card.status.rate !== null ? ' on' : ''}${step && !step.effective ? ' idle' : ''}`}
            title={step && !step.effective ? '此前已有更低的限速，这条不会生效' : undefined}
          >
            <span className="ntl-rule-k">达到</span>
            <AmountField card={card} index={index} />
            <span className="ntl-rule-k">限至</span>
            <RateField card={card} index={index} />
            {canEdit && (
              <button
                type="button"
                className="ntl-x"
                aria-label={`删除规则 ${index + 1}`}
                onClick={() => card.removeRule(index)}
              >
                <Icon of="close" size={12} />
              </button>
            )}
          </div>
        );
      })}
      {canEdit && form.limit.rules.length < 8 && (
        <button type="button" className="btn sm ntl-add" onClick={() => card.addRule()}>
          <Icon of="plus" size={12} />
          添加规则
        </button>
      )}
    </div>
  );
}

export function LimitSwitch({ card }: { card: Card }) {
  return (
    <SegSwitch
      checked={card.form.limit.enabled}
      disabled={!card.canEdit}
      off="关闭"
      on="开启"
      ariaLabel="机器自动限速开关"
      onChange={enabled =>
        card.setLimit({
          enabled,
          rules: enabled && card.form.limit.rules.length === 0 ? [DEFAULT_RULE] : card.form.limit.rules,
        })
      }
    />
  );
}

export function LayerSwitch({ card }: { card: Card }) {
  return (
    <SegmentedControl
      value={card.form.limit.layer}
      options={LAYERS.map(layer => ({ value: layer.value, label: layer.label }))}
      disabled={!card.canEdit}
      ariaLabel="限速执行层"
      onChange={layer => card.setLimit({ layer })}
    />
  );
}

/** 执行层下面的一行：默认写所选层的说明。两层分别回执、或有一层出错时改写各层结果——
 *  只有一层且正常时，结果与开关旁的状态是同一句话，不重复；过期的回执也不写在这里。 */
export function LayerNote({ card }: { card: Card }) {
  const { status } = card;
  const changed = card.form.limit.layer !== card.item.limit?.policy.layer;
  const worth = status.layers.length > 1 || status.layers.some(layer => layer.error !== null);
  if (changed || !status.confirmed || !worth)
    return <span className="sub">{LAYERS.find(layer => layer.value === card.form.limit.layer)?.note}</span>;
  return (
    <span className="sub ntl-layers">
      {card.status.layers.map(layer => (
        <span key={layer.layer} className={layer.tone ?? undefined}>
          {layer.name} {layer.text}
          {layer.rate !== null && ` ${rateText(layer.rate)}`}
          {layer.error && <em>：{layer.error}</em>}
        </span>
      ))}
    </span>
  );
}

/** 状态文字：已保存策略的执行结果，附回执时间（段标题里不附，窄屏会被截断）。 */
export function StatusText({ card, time = true }: { card: Card; time?: boolean }) {
  const { status } = card;
  if (status.text === '未启用') return null;
  return (
    <span
      className={`ntl-state${status.tone ? ` ${status.tone}` : ''}`}
      role="status"
      title={status.reportedAgo !== null && !time ? `机器回执于 ${agoText(status.reportedAgo)}` : undefined}
    >
      {status.text}
      {status.rate !== null && status.text === '已限速' && ` ${rateText(status.rate)}`}
      {time && status.reportedAgo !== null && status.confirmed && <small>{agoText(status.reportedAgo)}</small>}
    </span>
  );
}

/** 下一档：还差多少用量。 */
export function nextText(card: Card): string | null {
  const next = nextStep(card.steps);
  if (!next || card.used === null) return null;
  return `${next.label} 时 ${rateText(next.rate)}，还差 ${volume(next.at - card.used)}`;
}

export function Problem({ card }: { card: Card }) {
  return card.problem ? <p className="ntl-problem">{card.problem}</p> : null;
}

export function SaveBar({ card }: { card: Card }) {
  if (!card.dirty) return null;
  return (
    <div className="toolbar config-panel-savebar">
      <span className="sp" />
      <button type="button" className="btn primary" disabled={!card.canEdit || card.problem !== null}>
        保存
      </button>
      <button type="button" className="btn" onClick={card.revert}>
        还原
      </button>
    </div>
  );
}
