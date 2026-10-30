// Scenes and the derived limit model for the 流量统计 + 自动限速 mockup. All identities and readings are synthetic.
import type {
  NodeTrafficAccountingMode,
  NodeTrafficCycleKind,
  NodeTrafficItem,
  NodeTrafficLimitView,
  TrafficLayerObservation,
  TrafficLimitLayer,
  TrafficLimitPhase,
  TrafficLimitRule,
} from '../../src/api';
import { TRAFFIC_LIMIT_RECEIPT_FRESH_SECS } from '../../src/panes/nodes-traffic-limit';

export const DAY = 86_400;
const KiB = 1024;
const MiB = 1024 ** 2;
export const GiB = 1024 ** 3;
export const TiB = 1024 ** 4;
export const Kbps = 1_000;
export const Mbps = 1_000_000;

/** Fixed example clock (2026-10-22 09:00 UTC+8) keeps every scene reproducible. */
export const NOW = Date.UTC(2026, 9, 22, 1) / 1000;

/* ══ 读数格式 ══ */

const pad2 = (n: number) => String(n).padStart(2, '0');
export const md = (secs: number) => {
  const date = new Date(secs * 1000);
  return `${pad2(date.getUTCMonth() + 1)}-${pad2(date.getUTCDate())}`;
};
/** 最多三位小数，去掉末尾的 0。 */
export const trim = (value: number) => String(Number(value.toFixed(3)));

/** 主读数：两位小数，与 exactBytes 同一写法。 */
export function exact(n: number): string {
  const units: [string, number][] = [
    ['TiB', TiB],
    ['GiB', GiB],
    ['MiB', MiB],
    ['KiB', KiB],
  ];
  const unit = units.find(([, size]) => n >= size);
  return unit ? `${(n / unit[1]).toFixed(2)} ${unit[0]}` : `${Math.round(n)} B`;
}

/** 次要读数：三位有效数字，与流量预测页同一写法。 */
export function volume(n: number): string {
  if (n >= 999.5 * GiB) return `${(n / TiB).toFixed(2)} TiB`;
  if (n >= 99.95 * GiB) return `${(n / GiB).toFixed(0)} GiB`;
  if (n >= 9.995 * GiB) return `${(n / GiB).toFixed(1)} GiB`;
  if (n >= 999.5 * MiB) return `${(n / GiB).toFixed(2)} GiB`;
  if (n >= MiB) return `${(n / MiB).toFixed(0)} MiB`;
  return `${(n / KiB).toFixed(0)} KiB`;
}

/** 十进制比特速率，100 Mbps = 100,000 Kbps。 */
export function rateText(bps: number): string {
  if (bps >= 1e9) return `${trim(bps / 1e9)} Gbps`;
  if (bps >= Mbps) return `${trim(bps / Mbps)} Mbps`;
  return `${trim(bps / Kbps)} Kbps`;
}

export function agoText(secs: number): string {
  if (secs < 60) return `${Math.max(1, Math.round(secs))} 秒前`;
  if (secs < 3600) return `${Math.round(secs / 60)} 分钟前`;
  return `${Math.round(secs / 3600)} 小时前`;
}

/** 距重置：与生产卡同一写法，一天以内按小时，否则四舍五入到天。 */
export function untilText(secs: number): string {
  return secs < DAY ? `${Math.max(1, Math.round(secs / 3600))} 小时` : `${Math.round(secs / DAY)} 天`;
}

/* ══ 计量口径 ══ */

export const MODES: { value: NodeTrafficAccountingMode; label: string; note: string }[] = [
  { value: 'bidirectional_sum', label: '双向合计', note: '接收与发送相加。' },
  { value: 'bidirectional_max', label: '双向取大', note: '整个账期累计后取较大的方向，不按天取大。' },
  { value: 'outbound_only', label: '单向出站', note: '只计发送。' },
  { value: 'inbound_only', label: '单向入站', note: '只计接收。' },
];
export const modeLabel = (mode: NodeTrafficAccountingMode) => MODES.find(item => item.value === mode)?.label ?? mode;

export function countedDirections(item: NodeTrafficItem): { rx: boolean; tx: boolean } {
  switch (item.accounting_mode) {
    case 'bidirectional_sum':
      return { rx: true, tx: true };
    case 'inbound_only':
      return { rx: true, tx: false };
    case 'outbound_only':
      return { rx: false, tx: true };
    case 'bidirectional_max': {
      const rxLarger = Number(item.rx_bytes) >= Number(item.tx_bytes);
      return { rx: rxLarger, tx: !rxLarger };
    }
  }
}

function totalOf(mode: NodeTrafficAccountingMode, rx: number, tx: number): number {
  switch (mode) {
    case 'bidirectional_sum':
      return rx + tx;
    case 'bidirectional_max':
      return Math.max(rx, tx);
    case 'outbound_only':
      return tx;
    case 'inbound_only':
      return rx;
  }
}

/* ══ 执行层 ══ */

export const LAYERS: { value: TrafficLimitLayer; label: string; note: string }[] = [
  { value: 'application', label: '应用层', note: 'Xray 业务共用一个上限，系统与其他程序不受限。' },
  { value: 'system', label: '网卡', note: '默认出口网卡收、发分别限速，全部连接共享；需要 tc 与网络管理权限。' },
  { value: 'both', label: '双层', note: '网卡与 Xray 同时限速，两层分别确认结果。' },
];
export const layerLabel = (layer: TrafficLimitLayer) => LAYERS.find(item => item.value === layer)?.label ?? layer;

/* ══ 周期边界：与 NODE_TRAFFIC_VIEW_SQL 相同，短月取月末 ══ */

const daysIn = (year: number, month: number) => new Date(Date.UTC(year, month, 0)).getUTCDate();
const utcDay = (year: number, month: number, day: number) => Date.UTC(year, month - 1, day) / 1000;
const shiftMonth = (year: number, month: number, months: number): [number, number] => {
  const index = year * 12 + month - 1 + months;
  return [Math.floor(index / 12), (index % 12) + 1];
};

function periodOf(
  cycle: NodeTrafficCycleKind,
  resetMonth: number | null,
  resetDay: number,
  now: number,
): [number, number] {
  const today = new Date(now * 1000);
  const year = today.getUTCFullYear();
  const month = today.getUTCMonth() + 1;
  const todayStart = utcDay(year, month, today.getUTCDate());
  const anchor = ([y, m]: [number, number]) => utcDay(y, m, Math.min(resetDay, daysIn(y, m)));
  let base: [number, number];
  if (cycle === 'monthly') base = anchor([year, month]) <= todayStart ? [year, month] : shiftMonth(year, month, -1);
  else {
    const m = resetMonth ?? 1;
    base = anchor([year, m]) <= todayStart ? [year, m] : [year - 1, m];
  }
  return [anchor(base), anchor(shiftMonth(base[0], base[1], cycle === 'monthly' ? 1 : 12))];
}

/* ══ 规则：表单草稿 ↔ 协议 ══ */

export type ThresholdUnit = '%' | 'GiB' | 'TiB';
export type RateUnit = 'Kbps' | 'Mbps';
export interface DraftRule {
  amount: string;
  unit: ThresholdUnit;
  rate: string;
  rateUnit: RateUnit;
}

export const pct = (percent: number, rate: number): TrafficLimitRule => ({
  threshold: { kind: 'quota_percent', basis_points: Math.round(percent * 100) },
  rate_bps: rate,
});
export const abs = (bytes: number, rate: number): TrafficLimitRule => ({
  threshold: { kind: 'absolute', bytes: String(Math.round(bytes)) },
  rate_bps: rate,
});

export function draftOf(rule: TrafficLimitRule): DraftRule {
  const rateUnit: RateUnit = rule.rate_bps >= Mbps ? 'Mbps' : 'Kbps';
  const rate = trim(rule.rate_bps / (rateUnit === 'Mbps' ? Mbps : Kbps));
  if (rule.threshold.kind === 'quota_percent')
    return { amount: trim(rule.threshold.basis_points / 100), unit: '%', rate, rateUnit };
  const bytes = Number(rule.threshold.bytes);
  const unit: ThresholdUnit = bytes >= TiB ? 'TiB' : 'GiB';
  return { amount: trim(bytes / (unit === 'TiB' ? TiB : GiB)), unit, rate, rateUnit };
}

const DECIMAL = /^\d+(?:\.\d{1,3})?$/;
export const amountInvalid = (rule: DraftRule) => {
  const value = rule.amount.trim();
  return !DECIMAL.test(value) || Number(value) <= 0 || (rule.unit === '%' && Number(value) > 100);
};
export const rateInvalid = (rule: DraftRule) => {
  const value = rule.rate.trim();
  if (!DECIMAL.test(value)) return true;
  const bps = Number(value) * (rule.rateUnit === 'Mbps' ? Mbps : Kbps);
  return bps < Kbps || bps > 1e12;
};

/** 草稿转成协议规则；任一字段无效时为 null，不画到进度条上。 */
export function ruleOf(rule: DraftRule): TrafficLimitRule | null {
  if (amountInvalid(rule) || rateInvalid(rule)) return null;
  const rate = Math.round(Number(rule.rate) * (rule.rateUnit === 'Mbps' ? Mbps : Kbps));
  const amount = Number(rule.amount);
  return rule.unit === '%' ? pct(amount, rate) : abs(amount * (rule.unit === 'TiB' ? TiB : GiB), rate);
}

/* ══ 阶梯：规则按用量排开 ══ */

export interface Step {
  /** 规则在表单中的序号。 */
  index: number;
  /** 触发用量（字节）。 */
  at: number;
  rate: number;
  /** 阈值的原写法：百分比或绝对用量。 */
  label: string;
  /** 越过时会让上限更低；更早已有更低限速的规则永远不起作用。 */
  effective: boolean;
  reached: boolean;
  /** 决定当前上限的那一档。 */
  active: boolean;
}

/** 按触发用量排序。百分比规则在没有额度时无法落点，不画。命中多条时取最低速度，
 *  所以沿用量走一遍、只有让速度继续下降的规则才是有效台阶。 */
export function stepsOf(rules: (TrafficLimitRule | null)[], quota: number | null, used: number | null): Step[] {
  const placed: Step[] = [];
  rules.forEach((rule, index) => {
    if (!rule) return;
    const percent = rule.threshold.kind === 'quota_percent';
    if (percent && quota === null) return;
    const at =
      rule.threshold.kind === 'quota_percent'
        ? (quota! * rule.threshold.basis_points) / 10_000
        : Number(rule.threshold.bytes);
    const label =
      rule.threshold.kind === 'quota_percent' ? `${trim(rule.threshold.basis_points / 100)}%` : volume(at);
    placed.push({ index, at, rate: rule.rate_bps, label, effective: false, reached: false, active: false });
  });
  placed.sort((a, b) => a.at - b.at || a.rate - b.rate || a.index - b.index);
  let cap = Infinity;
  for (const step of placed) {
    if (step.rate < cap) {
      step.effective = true;
      cap = step.rate;
    }
    step.reached = used !== null && used >= step.at;
  }
  const active = [...placed].reverse().find(step => step.effective && step.reached);
  if (active) active.active = true;
  return placed;
}

export const nextStep = (steps: Step[]) => steps.find(step => step.effective && !step.reached) ?? null;

/* ══ 状态：已保存的策略 + 机器回执（与 MachineTrafficLimit 同一判定） ══ */

export type Tone = 'gold' | 'err' | 'mute';
const PHASE: Record<TrafficLimitPhase, string> = {
  disabled: '未启用',
  waiting: '等待应用确认',
  ready: '未达到阈值',
  limited: '已限速',
  error: '应用失败',
};

export interface LayerState {
  layer: 'system' | 'application';
  name: string;
  text: string;
  tone: Tone | null;
  rate: number | null;
  error: string | null;
}

export interface LimitStatus {
  /** 行内状态文字，沿用生产文案。 */
  text: string;
  tone: Tone | null;
  /** 标题栏的状态片；平稳状态（未启用、未达到阈值）不出现。 */
  chip: { text: string; tone: Tone } | null;
  confirmed: boolean;
  /** 机器确认的当前上限。 */
  rate: number | null;
  layers: LayerState[];
  reportedAgo: number | null;
}

export function statusOf(limit: NodeTrafficLimitView | undefined, now: number): LimitStatus {
  if (!limit)
    return {
      text: '控制面待升级',
      tone: 'mute',
      chip: null,
      confirmed: false,
      rate: null,
      layers: [],
      reportedAgo: null,
    };
  const { policy, observation } = limit;
  const reportedAgo = limit.reported_at_unix_secs === null ? null : now - limit.reported_at_unix_secs;
  const confirmed =
    !!observation &&
    observation.revision === limit.revision &&
    reportedAgo !== null &&
    reportedAgo < TRAFFIC_LIMIT_RECEIPT_FRESH_SECS;
  const layers: LayerState[] = [];
  if (observation)
    for (const layer of ['system', 'application'] as const) {
      const result: TrafficLayerObservation | null = observation[layer];
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
    reportedAgo,
    rate: confirmed && observation?.phase === 'limited' ? (observation.rate_bps ?? null) : null,
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
        chip: { text: `限速 ${rateText(observation!.rate_bps ?? 0)}`, tone: 'gold' },
      };
    case 'error':
      return { ...base, text: '应用失败', tone: 'err', chip: { text: '限速应用失败', tone: 'err' } };
    case 'waiting':
      return { ...base, text: '等待应用确认', tone: 'mute', chip: { text: '限速待确认', tone: 'mute' } };
    default:
      return { ...base, text: PHASE[observation!.phase], tone: null, chip: null };
  }
}

/* ══ 场景 ══ */

export interface Scene {
  label: string;
  note: string;
  item: NodeTrafficItem;
}

type LayerReport = [TrafficLimitPhase, number | null, string?];
interface LimitSpec {
  enabled: boolean;
  layer?: TrafficLimitLayer;
  rules?: TrafficLimitRule[];
  revision?: number;
  supported?: boolean;
  /** 机器最近一次回执；revision 缺省为当前修订。 */
  observed?: {
    revision?: number;
    phase: TrafficLimitPhase;
    rate?: number | null;
    error?: string | null;
    system?: LayerReport;
    application?: LayerReport;
    ago?: number;
  };
}
interface SceneSpec {
  label: string;
  note: string;
  quotaTiB: number | null;
  rx: number;
  tx: number;
  mode?: NodeTrafficAccountingMode;
  cycle?: NodeTrafficCycleKind;
  resetMonth?: number;
  resetDay?: number;
  tracked?: boolean;
  limit: LimitSpec;
}

function build(spec: SceneSpec): Scene {
  const mode = spec.mode ?? 'bidirectional_sum';
  const cycle = spec.cycle ?? 'monthly';
  const resetDay = spec.resetDay ?? 1;
  const resetMonth = cycle === 'yearly' ? (spec.resetMonth ?? 1) : null;
  const [start, end] = periodOf(cycle, resetMonth, resetDay, NOW);
  const tracked = spec.tracked ?? true;
  const rx = tracked ? Math.round(spec.rx) : 0;
  const tx = tracked ? Math.round(spec.tx) : 0;
  const total = totalOf(mode, rx, tx);
  const rules = spec.limit.rules ?? [];
  const revision = spec.limit.revision ?? (spec.limit.enabled || rules.length > 0 ? 3 : 0);
  const observed = spec.limit.observed;
  const layerOf = (report?: LayerReport): TrafficLayerObservation | null =>
    report ? { phase: report[0], rate_bps: report[1], error: report[2] ?? null } : null;
  return {
    label: spec.label,
    note: spec.note,
    item: {
      node_id: 'hk-01',
      tenant_id: 'platform',
      name: '香港示例 01',
      cycle_kind: cycle,
      accounting_mode: mode,
      reset_month: resetMonth,
      reset_day: resetDay,
      quota_bytes: spec.quotaTiB === null ? null : String(Math.round(spec.quotaTiB * TiB)),
      period_start_unix_secs: start,
      period_end_unix_secs: end,
      rx_bytes: String(rx),
      tx_bytes: String(tx),
      total_bytes: String(total),
      interface: tracked ? 'eth0' : null,
      tracking_started_at_unix_secs: tracked ? Date.UTC(2026, 8, 3) / 1000 : null,
      last_reported_at_unix_secs: tracked ? NOW - 20 : null,
      calibrated_at_unix_secs: null,
      last_gap_at_unix_secs: null,
      last_gap_reason: null,
      has_gap: false,
      limit: {
        revision,
        policy: { enabled: spec.limit.enabled, layer: spec.limit.layer ?? 'application', rules },
        supported: spec.limit.supported ?? true,
        observation: observed
          ? {
              revision: observed.revision ?? revision,
              phase: observed.phase,
              interface: 'eth0',
              rate_bps: observed.rate ?? null,
              used_bytes: String(total),
              period_start_unix_secs: start,
              error: observed.error ?? null,
              system: layerOf(observed.system),
              application: layerOf(observed.application),
            }
          : null,
        reported_at_unix_secs: observed ? NOW - (observed.ago ?? 40) : null,
      },
    },
  };
}

const TWO_STEPS = [pct(80, 100 * Mbps), pct(95, 10 * Mbps)];
const NO_PERMISSION = 'RTNETLINK answers: Operation not permitted';

export const SCENES = {
  armed: build({
    label: '已启用 · 未触发',
    note: '额度 2 TiB，已用 59%。80% 起限 100 Mbps，95% 起限 10 Mbps；应用层，机器已确认。',
    quotaTiB: 2,
    rx: 702 * GiB,
    tx: 516 * GiB,
    limit: { enabled: true, rules: TWO_STEPS, observed: { phase: 'ready', application: ['ready', null] } },
  }),
  limited: build({
    label: '已限速',
    note: '已用 85%，越过 80% 那一档；网卡与 Xray 两层都已限到 100 Mbps，到 95% 再降到 10 Mbps。',
    quotaTiB: 2,
    rx: 1044 * GiB,
    tx: 702 * GiB,
    limit: {
      enabled: true,
      layer: 'both',
      rules: TWO_STEPS,
      observed: {
        phase: 'limited',
        rate: 100 * Mbps,
        system: ['limited', 100 * Mbps],
        application: ['limited', 100 * Mbps],
      },
    },
  }),
  over: build({
    label: '超出额度',
    note: '已用 104%。80% 起 100 Mbps，100% 起 2 Mbps，当前 2 Mbps。',
    quotaTiB: 2,
    rx: 1228.8 * GiB,
    tx: 901.12 * GiB,
    limit: {
      enabled: true,
      rules: [pct(80, 100 * Mbps), pct(100, 2 * Mbps)],
      observed: { phase: 'limited', rate: 2 * Mbps, application: ['limited', 2 * Mbps] },
    },
  }),
  absolute: build({
    label: '无额度 · 绝对用量',
    note: '没有设额度，按单向出站计量。1 TiB 起 200 Mbps，3 TiB 起 20 Mbps；系统层。',
    quotaTiB: null,
    mode: 'outbound_only',
    rx: 2150 * GiB,
    tx: 1392 * GiB,
    limit: {
      enabled: true,
      layer: 'system',
      rules: [abs(1 * TiB, 200 * Mbps), abs(3 * TiB, 20 * Mbps)],
      observed: { phase: 'limited', rate: 200 * Mbps, system: ['limited', 200 * Mbps] },
    },
  }),
  partial: build({
    label: '一层失败',
    note: '双层保护：Xray 已限到 100 Mbps，网卡层缺少网络管理权限。',
    quotaTiB: 2,
    rx: 1044 * GiB,
    tx: 702 * GiB,
    limit: {
      enabled: true,
      layer: 'both',
      rules: TWO_STEPS,
      observed: {
        phase: 'error',
        rate: 100 * Mbps,
        error: NO_PERMISSION,
        system: ['error', null, NO_PERMISSION],
        application: ['limited', 100 * Mbps],
      },
    },
  }),
  pending: build({
    label: '等待确认',
    note: '刚把第一档改到 75%，机器的回执还是上一修订。',
    quotaTiB: 2,
    rx: 702 * GiB,
    tx: 516 * GiB,
    limit: {
      enabled: true,
      revision: 4,
      rules: [pct(75, 100 * Mbps), pct(95, 10 * Mbps)],
      observed: { revision: 3, phase: 'ready', application: ['ready', null], ago: 12 },
    },
  }),
  releasing: build({
    label: '关闭后解除中',
    note: '刚关闭自动限速，机器仍在 100 Mbps，等待解除回执。',
    quotaTiB: 2,
    rx: 1044 * GiB,
    tx: 702 * GiB,
    limit: {
      enabled: false,
      revision: 5,
      rules: TWO_STEPS,
      observed: { revision: 4, phase: 'limited', rate: 100 * Mbps, application: ['limited', 100 * Mbps], ago: 8 },
    },
  }),
  waiting: build({
    label: '等待计量基线',
    note: '校准之后，机器还没有建立本期的计量基线。',
    quotaTiB: 2,
    rx: 702 * GiB,
    tx: 516 * GiB,
    limit: { enabled: true, rules: TWO_STEPS, observed: { phase: 'waiting', application: ['waiting', null] } },
  }),
  handoff: build({
    label: '等待旧代确认',
    note: '网卡已经执行，Xray 正常交接中，尚未确认整机上限。',
    quotaTiB: 2,
    rx: 1200 * GiB,
    tx: 600 * GiB,
    limit: {
      enabled: true,
      layer: 'both',
      rules: TWO_STEPS,
      observed: {
        phase: 'waiting',
        rate: 100 * Mbps,
        system: ['limited', 100 * Mbps],
        application: ['waiting', null, '等待旧代排空后再确认应用层上限'],
      },
    },
  }),
  heartbeat: build({
    label: '确认有效期内',
    note: '上次上报已过四分钟，当前修订的确认仍在 300 秒有效期内。',
    quotaTiB: 2,
    rx: 1200 * GiB,
    tx: 600 * GiB,
    limit: {
      enabled: true,
      rules: TWO_STEPS,
      observed: { phase: 'limited', rate: 100 * Mbps, application: ['limited', 100 * Mbps], ago: 240 },
    },
  }),
  upgrade: build({
    label: 'Agent 待升级',
    note: '策略已保存，但这台机器的 Agent 还不支持自动限速。',
    quotaTiB: 2,
    rx: 702 * GiB,
    tx: 516 * GiB,
    limit: { enabled: true, supported: false, rules: TWO_STEPS },
  }),
  off: build({
    label: '未启用',
    note: '额度 2 TiB，已用 41%，没有限速规则。',
    quotaTiB: 2,
    rx: 520 * GiB,
    tx: 320 * GiB,
    limit: { enabled: false },
  }),
  dense: build({
    label: '8 条规则',
    note: '规则上限 8 条，额度 4 TiB。62% 那条高于此前的 500 Mbps，永远不会生效；3.5 TiB 是绝对用量。',
    quotaTiB: 4,
    rx: 1.6 * TiB,
    tx: 1.02 * TiB,
    limit: {
      enabled: true,
      rules: [
        pct(50, 800 * Mbps),
        pct(60, 500 * Mbps),
        pct(62, 600 * Mbps),
        pct(70, 300 * Mbps),
        pct(80, 100 * Mbps),
        abs(3.5 * TiB, 80 * Mbps),
        pct(90, 50 * Mbps),
        pct(95, 10 * Mbps),
      ],
      observed: { phase: 'limited', rate: 500 * Mbps, application: ['limited', 500 * Mbps] },
    },
  }),
  yearly: build({
    label: '年度重置',
    note: '每年 3 月 15 日重置，额度 20 TiB；90% 起限 50 Mbps。',
    quotaTiB: 20,
    cycle: 'yearly',
    resetMonth: 3,
    resetDay: 15,
    rx: 6.9 * TiB,
    tx: 4.5 * TiB,
    limit: {
      enabled: true,
      rules: [pct(90, 50 * Mbps)],
      observed: { phase: 'ready', application: ['ready', null] },
    },
  }),
  untracked: build({
    label: '尚未上报',
    note: '新机器，Agent 还没有上报网卡累计；规则已保存。',
    quotaTiB: 2,
    rx: 0,
    tx: 0,
    tracked: false,
    limit: {
      enabled: true,
      rules: [pct(80, 100 * Mbps)],
      observed: { phase: 'waiting', application: ['waiting', null] },
    },
  }),
} satisfies Record<string, Scene>;

export type SceneKey = keyof typeof SCENES;
export const SCENE_KEYS = Object.keys(SCENES) as SceneKey[];
