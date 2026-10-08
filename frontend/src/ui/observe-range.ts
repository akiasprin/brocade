/** 机器详情观测页的时间范围。
 *
 * 快速范围、固定区间、地址栏查询段，以及图上拖选和双击缩小的取值规则都在这里定义。路由
 * （forge/route.ts）按这里的规则规范化地址，机器详情页按它解析地址、切换范围，图表拖选按它
 * 取整与补足：三处共用一份定义，地址里写下的范围与页面显示、发给接口的边界一致。
 *
 * 纯函数模块，不读时钟：需要「当前时间」的函数由调用方传入，渲染期与事件处理期各自决定时刻。
 */

export interface LoadRange {
  seconds: number;
  label: string;
  menuLabel: string;
  heading: string;
  /** 存在时是固定历史区间；没有时是随当前时间移动的最近 N 秒。 */
  startUnixSecs?: number;
  endUnixSecs?: number;
}

/** 快速范围。`label` 同时是地址栏 `?range=` 的取值。 */
export const LOAD_RANGES = [
  { seconds: 30 * 60, label: '30m', menuLabel: '近 30 分钟', heading: '30 MINUTES' },
  { seconds: 60 * 60, label: '1h', menuLabel: '近 1 小时', heading: '1 HOUR' },
  { seconds: 6 * 60 * 60, label: '6h', menuLabel: '近 6 小时', heading: '6 HOURS' },
  { seconds: 12 * 60 * 60, label: '12h', menuLabel: '近 12 小时', heading: '12 HOURS' },
  { seconds: 24 * 60 * 60, label: '24h', menuLabel: '近 24 小时', heading: '24 HOURS' },
] as const;
export const DEFAULT_LOAD_RANGE: LoadRange = LOAD_RANGES[1];

/** 固定区间的上下限，与服务端 `validate_telemetry_range` 的跨度限制一致。 */
export const MIN_LOAD_RANGE_SECS = 60;
export const MAX_LOAD_RANGE_SECS = 24 * 60 * 60;

export function fixedLoadRange(range: LoadRange): boolean {
  return range.startUnixSecs != null && range.endUnixSecs != null;
}

/** 查询缓存与范围比较用的键：固定区间为「起-止」，快速范围为秒数。 */
export function loadRangeKey(range: LoadRange): string | number {
  return range.startUnixSecs != null && range.endUnixSecs != null
    ? `${range.startUnixSecs}-${range.endUnixSecs}`
    : range.seconds;
}

const two = (value: number) => String(value).padStart(2, '0');

/** 本地时刻 HH:MM 或 HH:MM:SS。不用 toLocaleTimeString：部分引擎在 hour12: false 下把零点写成 24:00。 */
export function observeClockText(unixSecs: number, withSeconds: boolean): string {
  const date = new Date(unixSecs * 1000);
  const minutes = `${two(date.getHours())}:${two(date.getMinutes())}`;
  return withSeconds ? `${minutes}:${two(date.getSeconds())}` : minutes;
}

const monthDay = (unixSecs: number) => {
  const date = new Date(unixSecs * 1000);
  return `${two(date.getMonth() + 1)}/${two(date.getDate())}`;
};

/** 本地日期的零点（unix 秒）。 */
export function localDayStart(unixSecs: number): number {
  const date = new Date(unixSecs * 1000);
  date.setHours(0, 0, 0, 0);
  return Math.floor(date.getTime() / 1000);
}

const sameLocalDay = (left: number, right: number) => localDayStart(left) === localDayStart(right);

/** 区间读数「起 → 止」。止与起同一天时只写一次日期；`date` 为 false 时两端都不写日期。 */
export function observeSpanText(
  startUnixSecs: number,
  endUnixSecs: number,
  { seconds, date }: { seconds: boolean; date: boolean },
): string {
  const start = `${date ? `${monthDay(startUnixSecs)} ` : ''}${observeClockText(startUnixSecs, seconds)}`;
  const endDate = date && !sameLocalDay(startUnixSecs, endUnixSecs) ? `${monthDay(endUnixSecs)} ` : '';
  return `${start} → ${endDate}${observeClockText(endUnixSecs, seconds)}`;
}

/** 时长读数：「4 分 20 秒」「7 分钟」「1 小时 12 分」「2 小时」。 */
export function observeDurationText(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;
  if (hours > 0) return minutes > 0 ? `${hours} 小时 ${minutes} 分` : `${hours} 小时`;
  return rest > 0 ? `${minutes} 分 ${rest} 秒` : `${minutes} 分钟`;
}

/** 起止都在整分时只显示到分。 */
const needsSeconds = (startUnixSecs: number, endUnixSecs: number) => startUnixSecs % 60 !== 0 || endUnixSecs % 60 !== 0;

/** 固定区间。`menuLabel` 带日期，用在提示与错误说明里；页头按钮的读数见 `loadRangeTriggerText`。 */
export function customLoadRange(startUnixSecs: number, endUnixSecs: number): LoadRange {
  return {
    seconds: endUnixSecs - startUnixSecs,
    label: 'custom',
    menuLabel: observeSpanText(startUnixSecs, endUnixSecs, {
      seconds: needsSeconds(startUnixSecs, endUnixSecs),
      date: true,
    }),
    heading: 'CUSTOM RANGE',
    startUnixSecs,
    endUnixSecs,
  };
}

/** 页头时间按钮的读数。固定区间起止都在今天时省略日期：窄屏页头里时间按钮与机器名共用一行，
 * 带日期的读数会把机器名截成省略号。 */
export function loadRangeTriggerText(range: LoadRange, todayStartUnixSecs: number): string {
  if (range.startUnixSecs == null || range.endUnixSecs == null) return range.menuLabel;
  const today =
    localDayStart(range.startUnixSecs) === todayStartUnixSecs && sameLocalDay(range.startUnixSecs, range.endUnixSecs);
  return today
    ? observeSpanText(range.startUnixSecs, range.endUnixSecs, {
        seconds: needsSeconds(range.startUnixSecs, range.endUnixSecs),
        date: false,
      })
    : range.menuLabel;
}

/* ══ 地址栏查询段 ══
 *   近 1 小时（默认）  不写
 *   其余快速范围        ?range=6h
 *   固定区间            ?from=<unix 秒>&to=<unix 秒>
 */

/* 类型别名而不是接口：要并入路由的下钻状态（Record<string, unknown>），接口没有隐式索引签名。 */
export type LoadRangeQuery = {
  range?: string;
  from?: number;
  to?: number;
};

export function loadRangeQuery(range: LoadRange): LoadRangeQuery {
  if (range.startUnixSecs != null && range.endUnixSecs != null) {
    return { from: range.startUnixSecs, to: range.endUnixSecs };
  }
  return range.seconds === DEFAULT_LOAD_RANGE.seconds ? {} : { range: range.label };
}

const validFixedSpan = (from: unknown, to: unknown): from is number =>
  typeof from === 'number' &&
  typeof to === 'number' &&
  Number.isSafeInteger(from) &&
  Number.isSafeInteger(to) &&
  from >= 0 &&
  to - from >= MIN_LOAD_RANGE_SECS &&
  to - from <= MAX_LOAD_RANGE_SECS;

/** 地址栏解析出的查询值 → 范围。非法组合按默认范围处理；`canonicalLoadRangeQuery` 已在路由层去掉它们。 */
export function loadRangeFromQuery(query: { range?: unknown; from?: unknown; to?: unknown }): LoadRange {
  if (query.from !== undefined || query.to !== undefined) {
    return validFixedSpan(query.from, query.to) ? customLoadRange(query.from, query.to as number) : DEFAULT_LOAD_RANGE;
  }
  return LOAD_RANGES.find(option => option.label === query.range) ?? DEFAULT_LOAD_RANGE;
}

/** 规范化下钻状态里的范围字段：固定区间优先；非法组合、默认范围与未知快速范围都去掉。
 * 路由在解析与写出地址时都经过这里，因此手改出的地址会被改写为规范形式。 */
export function canonicalLoadRangeQuery(drill: Record<string, unknown>): Record<string, unknown> {
  const { range, from, to, ...rest } = drill;
  if (from !== undefined || to !== undefined) return validFixedSpan(from, to) ? { ...rest, from, to } : rest;
  const preset = LOAD_RANGES.find(option => option.label === range);
  return preset && preset.seconds !== DEFAULT_LOAD_RANGE.seconds ? { ...rest, range } : rest;
}

/* ══ 拖选与缩小 ══ */

/** 拖选起止的取整单位（秒）。 */
export const OBSERVE_SNAP_STEPS = [1, 5, 10, 30, 60, 300, 600] as const;

/** 不小于一个像素对应时长的最小一档：近 1 小时约 10 秒，近 24 小时 5 分钟。 */
export function observeSnapStep(secondsPerPixel: number): number {
  return OBSERVE_SNAP_STEPS.find(step => step >= secondsPerPixel) ?? OBSERVE_SNAP_STEPS[OBSERVE_SNAP_STEPS.length - 1];
}

export interface ObserveSelection {
  startUnixSecs: number;
  endUnixSecs: number;
  /** 取整单位；小于 1 分钟时读数显示到秒。 */
  step: number;
  /** 拖动不足 1 分钟，已按 1 分钟补足。 */
  widened: boolean;
}

/** 拖动两端 → 应用的区间：按取整单位取整；不足 1 分钟时以中点为中心补足；结束不晚于当前时间。 */
export function observeSelection(
  anchorUnixSecs: number,
  pointerUnixSecs: number,
  step: number,
  nowUnixSecs: number,
): ObserveSelection {
  const low = Math.min(anchorUnixSecs, pointerUnixSecs);
  const high = Math.max(anchorUnixSecs, pointerUnixSecs);
  let start = Math.round(low / step) * step;
  let end = Math.round(high / step) * step;
  let widened = false;
  if (end - start < MIN_LOAD_RANGE_SECS) {
    const span = Math.ceil(MIN_LOAD_RANGE_SECS / step) * step;
    start = Math.round(((low + high) / 2 - span / 2) / step) * step;
    end = start + span;
    widened = true;
  }
  if (end > nowUnixSecs) {
    start -= end - nowUnixSecs;
    end = nowUnixSecs;
  }
  return { startUnixSecs: start, endUnixSecs: end, step, widened };
}

/** 双击缩小：跨度翻倍、两侧各加一半，结束不晚于当前时间，最长 24 小时。已到上限时返回 null。 */
export function observeZoomOut(
  startUnixSecs: number,
  endUnixSecs: number,
  nowUnixSecs: number,
): { startUnixSecs: number; endUnixSecs: number } | null {
  const span = endUnixSecs - startUnixSecs;
  const nextSpan = Math.min(MAX_LOAD_RANGE_SECS, span * 2);
  const nextEnd = Math.min(nowUnixSecs, Math.round((startUnixSecs + endUnixSecs) / 2 + nextSpan / 2));
  if (nextSpan === span && nextEnd === endUnixSecs) return null;
  return { startUnixSecs: nextEnd - nextSpan, endUnixSecs: nextEnd };
}
