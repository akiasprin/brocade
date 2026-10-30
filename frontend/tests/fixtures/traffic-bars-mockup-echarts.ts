// Mockup only: restyles the production bar charts per option. Never imported by the app.
// SiteUsageChart (流出统计) and ThroughputChart (历史流量) still build their own options; this module wraps
// the ECharts instance methods and rewrites those options before they reach the canvas, so 现状 and every
// option run the same production cards on the same data. When an option lands, these rewrites move into
// site-usage-chart.tsx and panes/telemetry.tsx.
import * as echarts from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { theme } from '../../src/forge/theme';
import { throughputAxis } from '../../src/panes/telemetry-format';
import { bytes } from '../../src/ui/format';
import { historyBucketMillis } from './traffic-bars-mockup-data';

// Browser checks inspect the very instance rendered by the production card.
export { getInstanceByDom } from 'echarts/core';

export type BarOption = 'current' | 'a' | 'b' | 'c';
let option: BarOption = 'current';
export function setBarOption(next: BarOption) {
  option = next;
}

/** Opacity left to the other categories while one is in focus. Production inherits ECharts' 0.1. */
export const DIM = 0.5;
/** Stack position of the markers: their own empty series, so focus never fades 「现在」. */
const MARKS = '__marks';

type Chart = ReturnType<typeof echarts.init>;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Loose = Record<string, any>;
type Method = (this: Chart, ...args: unknown[]) => unknown;

// The production charts register the same renderer when their lazy modules load.
echarts.use([CanvasRenderer]);
const probe = echarts.init(document.createElement('div'), null, { renderer: 'canvas', width: 8, height: 8 });
const proto = Object.getPrototypeOf(probe) as { setOption: Method; dispatchAction: Method; resize: Method };
probe.dispose();
const setOption = proto.setOption;
const dispatchAction = proto.dispatchAction;
const resize = proto.resize;

/* ── 颜色 ── */

type Rgba = [number, number, number, number];
function rgbaOf(color: string): Rgba | null {
  const text = color.trim();
  const hex = /^#([0-9a-f]{6})$/i.exec(text);
  if (hex) {
    const value = Number.parseInt(hex[1], 16);
    return [(value >> 16) & 255, (value >> 8) & 255, value & 255, 1];
  }
  const fn = /^rgba?\(([^)]+)\)$/i.exec(text);
  if (!fn) return null;
  const [r, g, b, a = 1] = fn[1].split(',').map(part => Number.parseFloat(part));
  return [r, g, b, a];
}
const hexOf = (r: number, g: number, b: number) =>
  `#${[r, g, b].map(value => Math.round(value).toString(16).padStart(2, '0')).join('')}`;
/** `share` of `top` over `bottom`, flattened to an opaque color. */
function mix(top: string, bottom: string, share: number): string {
  const a = rgbaOf(top);
  const b = rgbaOf(bottom);
  if (!a || !b) return top;
  const t = share * a[3];
  return hexOf(a[0] * t + b[0] * (1 - t), a[1] * t + b[1] * (1 - t), a[2] * t + b[2] * (1 - t));
}
const flatten = (color: string, paper: string) => mix(color, paper, 1);
function isNeutral(color: string) {
  const c = rgbaOf(color);
  return !!c && Math.max(c[0], c[1], c[2]) - Math.min(c[0], c[1], c[2]) < 28;
}
const alpha = (color: string, share: number) => {
  const c = rgbaOf(color);
  return c ? `rgba(${c[0]}, ${c[1]}, ${c[2]}, ${share})` : color;
};

function tokens() {
  const css = getComputedStyle(document.documentElement);
  const cv = (name: string) => css.getPropertyValue(name).trim();
  return {
    card: cv('--card'),
    ink: cv('--ink'),
    ink3: cv('--ink-3'),
    ink4: cv('--ink-4'),
    line: cv('--line-soft'),
    slot3: cv('--observe-3'),
    slot7: cv('--observe-7'),
    light: theme.snapshot() === 'light',
  };
}
const escapeHtml = (text: string) =>
  text.replace(/[&<>"]/g, char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[char]!);

/* ══ 流出统计（SiteUsageChart） ══ */

interface Layer {
  id: string;
  name: string;
  color: string;
  neutral: boolean;
  values: (number | null)[];
}
interface SiteState {
  base: Loose;
  layers: Layer[];
  marks: { markLine?: Loose; markArea?: Loose };
  areaLabel?: boolean;
  legend: string | null;
  hover: string | null;
  order: string;
}
const siteStates = new WeakMap<Chart, SiteState>();
const wired = new WeakSet<Chart>();
const scheduled = new WeakSet<Chart>();
const isSiteChart = (chart: Chart) => chart.getDom()?.classList.contains('site-usage-chart') === true;

function siteSeries(chart: Chart, state: SiteState) {
  const t = tokens();
  const focus = state.hover ?? state.legend;
  const yMax = Number(state.base.yAxis?.max) || 1;
  // Grid top 20 + bottom 6 + the x labels: close enough to decide which segments get a separator.
  const plot = Math.max(60, chart.getHeight() - 50);
  const px = (value: number | null) => ((value ?? 0) / yMax) * plot;
  // A card-colored hairline between segments. Segments under 3 px keep their full height instead, and the
  // hourly bars of a week (about 4 px wide) stay solid: a hairline on each side would leave a sliver.
  const buckets = state.layers[0]?.values.length ?? 1;
  const wide = ((chart.getWidth() - 60) / buckets) * 0.72 >= 8;
  const separated = (value: number | null) => ({
    borderColor: t.card,
    borderWidth: wide && px(value) >= 3 ? 1 : 0,
  });
  const bar = (id: string, name: string, values: (number | null)[], itemStyle: Loose, separate: boolean): Loose => ({
    id,
    name,
    type: 'bar',
    stack: 'sites',
    barWidth: '72%',
    emphasis: { disabled: true },
    itemStyle,
    data: values.map((value, bucket) => ({
      value: [bucket + 0.5, value],
      itemStyle: separate ? separated(value) : { borderWidth: 0 },
    })),
  });
  let series: Loose[];
  // The list's focus in B: the others become one gray block below and one above the focused category.
  // Pointing into the chart is a light touch in every option, so the chart keeps its layers then.
  const grayed = option === 'b' && state.hover === null && focus !== null;
  const at = state.layers.findIndex(layer => layer.id === focus);
  if (grayed && at >= 0) {
    const gray = mix(t.ink, t.card, t.light ? 0.15 : 0.24);
    const lifted = mix(t.ink, t.card, t.light ? 0.6 : 0.7);
    const layer = state.layers[at];
    const sum = (layers: Layer[]) =>
      layer.values.map((_, bucket) =>
        layers.every(item => item.values[bucket] === null)
          ? null
          : layers.reduce((total, item) => total + (item.values[bucket] ?? 0), 0),
      );
    const below = state.layers.slice(0, at);
    const above = state.layers.slice(at + 1);
    series = [
      ...(below.length ? [bar('__below', '', sum(below), { color: gray }, false)] : []),
      // A gray category in focus would vanish among the grayed others, so it is lifted.
      bar(layer.id, layer.name, layer.values, { color: layer.neutral ? lifted : layer.color }, true),
      ...(above.length ? [bar('__above', '', sum(above), { color: gray }, false)] : []),
    ];
  } else {
    // C moves the category the list points at to the baseline. Hovering the chart itself never reorders:
    // the segment would slide away from under the pointer.
    const rebase = option === 'c' && state.hover === null ? state.legend : null;
    const ids = state.layers.map(layer => layer.id);
    const order = rebase && ids.includes(rebase) ? [rebase, ...ids.filter(id => id !== rebase)] : ids;
    series = order.map(id => {
      const layer = state.layers.find(item => item.id === id)!;
      const dimmed = focus !== null && layer.id !== focus;
      return bar(layer.id, layer.name, layer.values, { color: layer.color, opacity: dimmed ? DIM : 1 }, true);
    });
  }
  const area = state.marks.markArea;
  series.push({
    id: MARKS,
    type: 'bar',
    stack: 'sites',
    silent: true,
    data: [],
    markLine: state.marks.markLine,
    markArea: area && { ...area, label: { ...area.label, show: state.areaLabel ?? area.label?.show } },
  });
  state.order = series.map(item => item.id).join('\n');
  return series;
}

function siteTip(chart: Chart, params: unknown, production: (params: unknown) => string): string {
  const html = production(params);
  const state = siteStates.get(chart);
  if (!html || !state) return html;
  const list = params as { seriesId?: string; value?: [number, number | null] }[];
  const value = new Map(list.map(item => [String(item.seriesId), item.value?.[1] ?? 0]));
  const rows = state.layers.filter(layer => (value.get(layer.id) ?? 0) > 0);
  if (rows.length === 0) return html;
  const t = tokens();
  const head = /^<div[^>]*>[\s\S]*?<\/div>/.exec(html)?.[0] ?? '';
  const focus = state.hover ?? state.legend;
  const total = rows.reduce((sum, layer) => sum + (value.get(layer.id) ?? 0), 0);
  const row = (swatch: string, name: string, amount: string, strong: boolean) =>
    `<div style="display:flex;gap:16px;justify-content:space-between;align-items:center;line-height:1.72">` +
    `<span style="display:flex;align-items:center;gap:6px;min-width:0;color:${strong ? t.ink : t.ink3}">${swatch}${name}</span>` +
    `<b style="color:${t.ink};font-weight:${strong ? 600 : 500}">${amount}</b></div>`;
  const dot = (color: string) =>
    `<i style="display:inline-block;width:7px;height:7px;border-radius:2px;background:${color};flex:none"></i>`;
  return (
    head +
    `<div style="margin-bottom:3px;padding-bottom:3px;border-bottom:1px solid ${alpha(t.ink, 0.1)}">${row('', '合计', bytes(total), true)}</div>` +
    rows
      .map(layer => row(dot(layer.color), escapeHtml(layer.name), bytes(value.get(layer.id)!), layer.id === focus))
      .join('')
  );
}

function syncLegend(chart: Chart, state: SiteState) {
  const card = chart.getDom()?.closest('.site-usage');
  if (!card) return;
  const name = state.hover ? state.layers.find(layer => layer.id === state.hover)?.name : undefined;
  for (const item of card.querySelectorAll<HTMLElement>('.site-usage-key > .site-usage-item')) {
    const text = item.querySelector('.site-usage-name')?.textContent?.trim();
    item.classList.toggle('tb-hot', !!name && text === name && !item.classList.contains('sub'));
  }
}

function siteFocus(chart: Chart, state: SiteState) {
  const before = state.order;
  const series = siteSeries(chart, state);
  if (state.order !== before && before !== '') {
    // C reorders the stack and B swaps in its gray blocks. Merging keeps ECharts' original stack order,
    // so the chart is rebuilt; the pointer is on the list at this moment, so no tooltip is lost.
    setOption.call(chart, { ...siteOption(chart, state, series), animation: false }, true);
  } else {
    setOption.call(chart, { animation: false, series });
  }
  syncLegend(chart, state);
}

function wire(chart: Chart) {
  if (wired.has(chart)) return;
  wired.add(chart);
  const hover = (id: string | null) => {
    const state = siteStates.get(chart);
    if (!state || option === 'current' || state.hover === id) return;
    state.hover = id;
    siteFocus(chart, state);
  };
  chart.on('mouseover', (event: Loose) => {
    // Ids starting with __ are this mockup's own series: the markers and B's gray blocks.
    if (event.componentType === 'series' && !String(event.seriesId).startsWith('__')) hover(String(event.seriesId));
  });
  chart.on('mouseout', (event: Loose) => {
    if (event.componentType === 'series') hover(null);
  });
  chart.on('globalout', () => hover(null));
}

/** The production option with this mockup's tooltip, axis unit and series. */
function siteOption(chart: Chart, state: SiteState, series: Loose[]): Loose {
  const t = tokens();
  const base = state.base;
  const label = base.yAxis.axisLabel.formatter as (value: number) => string;
  const unit = label(base.yAxis.max).split(' ')[1] ?? '';
  const productionTip = base.tooltip.formatter as (params: unknown) => string;
  return {
    ...base,
    tooltip: {
      ...base.tooltip,
      axisPointer: { type: 'shadow', shadowStyle: { color: alpha(t.ink, t.light ? 0.05 : 0.055) }, z: 0 },
      formatter: (params: unknown) => siteTip(chart, params, productionTip),
    },
    yAxis: {
      ...base.yAxis,
      name: unit,
      nameLocation: 'end',
      nameGap: 9,
      nameTextStyle: { color: t.ink3, fontSize: 9.5, align: 'right', padding: [0, 7, 0, 0] },
      axisLabel: {
        ...base.yAxis.axisLabel,
        formatter: (value: number) => (value === 0 ? '0' : label(value).split(' ')[0]),
      },
    },
    series,
  };
}

function siteFull(chart: Chart, base: Loose, rest: unknown[]) {
  const t = tokens();
  const prior = siteStates.get(chart);
  const production = base.series as Loose[];
  const layers: Layer[] = production.map(series => {
    const raw = String(series.itemStyle?.color ?? '');
    // 第 5 类站点：观测色第 7 档在黛蓝、玄墨等调色盘里与端口转发（第 2 档）几乎同色，改用第 3 档。
    const remapped = raw.toLowerCase() === t.slot7.toLowerCase() ? t.slot3 : raw;
    const color = flatten(remapped, t.card);
    return {
      id: String(series.id),
      name: String(series.name),
      color,
      neutral: isNeutral(color),
      values: (series.data as [number, number | null][]).map(point => point[1]),
    };
  });
  const state: SiteState = {
    base,
    layers,
    marks: { markLine: production[0]?.markLine, markArea: production[0]?.markArea },
    legend: prior?.legend ?? null,
    hover: null,
    order: '',
  };
  siteStates.set(chart, state);
  wire(chart);
  setOption.call(chart, siteOption(chart, state, siteSeries(chart, state)), ...rest);
  syncLegend(chart, state);
}

/* ══ 历史流量（ThroughputChart 柱状） ══ */

interface HistoryState {
  start: number;
  bucketMillis: number;
  names: string[];
}
const historyStates = new WeakMap<Chart, HistoryState>();

/** Use the laid-out axis, whose width changes with the unit, theme and container size. */
function fitHistoryBars(chart: Chart, state: HistoryState) {
  const x = (at: number) => chart.convertToPixel({ xAxisIndex: 0 }, at);
  const band = Math.abs(x(state.start + state.bucketMillis) - x(state.start));
  if (!Number.isFinite(band) || band <= 0) return;
  // Cover fractional-pixel seams, while leaving at least 3/4 of an absent window empty.
  const width = band + Math.min(0.6, band / 4);
  setOption.call(chart, { series: state.names.map(name => ({ name, barWidth: width })) });
}

/** Every bar is one reported window; missing samples must never widen its neighbours. */
function historyBars(chart: Chart, base: Loose, rest: unknown[]) {
  const axis = Array.isArray(base.xAxis) ? base.xAxis[0] : base.xAxis;
  const bars = (base.series as Loose[]).filter(series => series.type === 'bar');
  const totals = new Map<number, number>();
  for (const series of bars) {
    for (const [time, value] of series.data as [number, number | null][]) {
      if (value !== null) totals.set(time, (totals.get(time) ?? 0) + value);
    }
  }
  const { axis: valueAxis, unit } = throughputAxis([...totals.values()], []);
  const yAxis = Array.isArray(base.yAxis) ? base.yAxis[0] : base.yAxis;
  const state: HistoryState = {
    start: Number(axis.min),
    bucketMillis: historyBucketMillis(Number(axis.min), Number(axis.max)),
    names: bars.map(series => String(series.name)),
  };
  historyStates.set(chart, state);
  const unitLabel = chart.getDom()?.closest('.rut-hour')?.querySelector('.chart-unit');
  if (unitLabel) unitLabel.textContent = `(${unit.name})`;
  const next = {
    ...base,
    yAxis: {
      ...yAxis,
      ...valueAxis,
      axisLabel: { ...yAxis.axisLabel, formatter: unit.text },
    },
    tooltip: {
      ...base.tooltip,
      formatter: (params: unknown) => {
        const list = params as { seriesName: string; color: string; value: [number, number | null] }[];
        if (!list.length) return '';
        const t = tokens();
        const time = new Date(list[0].value[0]).toLocaleTimeString('zh-CN', { hour12: false });
        const rows = [...list]
          .sort((a, b) => (b.value[1] ?? -Infinity) - (a.value[1] ?? -Infinity))
          .map(
            item =>
              `<div style="display:flex;gap:7px;align-items:center;line-height:1.75">` +
              `<span style="width:8px;height:8px;border-radius:2px;background:${item.color};flex:none"></span>` +
              `<span style="color:${t.ink3}">${escapeHtml(item.seriesName)}</span>` +
              `<b style="margin-left:auto;color:${t.ink};font-weight:500">${item.value[1] === null ? '—' : unit.read(item.value[1])}</b></div>`,
          )
          .join('');
        return `<div style="color:${t.ink4};font-size:9px;margin-bottom:4px;letter-spacing:.04em">${time}</div>${rows}`;
      },
    },
    series: (base.series as Loose[]).map(series =>
      series.type !== 'bar'
        ? series
        : { ...series, stack: 'traffic', barMaxWidth: undefined, barGap: '0%', barCategoryGap: '0%' },
    ),
  };
  const result = setOption.call(chart, next, ...rest);
  fitHistoryBars(chart, state);
  return result;
}
const isHistoryBars = (chart: Chart) => !!chart.getDom()?.closest('.ndtp-bars');

/* ── 包装实例方法 ── */

proto.setOption = function (this: Chart, ...args: unknown[]) {
  const [next, ...rest] = args as [Loose | undefined, ...unknown[]];
  if (option === 'current' || !next || typeof next !== 'object' || !Array.isArray(next.series))
    return setOption.apply(this, args);
  if (isSiteChart(this)) {
    if (next.series.some((series: Loose) => series.stack === 'sites')) return siteFull(this, next, rest);
    // Production fits the 未观测 label by writing to its first category; the marks live on MARKS here.
    if (next.series.every((series: Loose) => series.markArea && !series.data)) {
      const state = siteStates.get(this);
      if (state) state.areaLabel = next.series[0].markArea?.label?.show;
      return setOption.call(this, { series: [{ id: MARKS, markArea: next.series[0].markArea }] });
    }
  }
  if (isHistoryBars(this) && next.series.some((series: Loose) => series.type === 'bar'))
    return historyBars(this, next, rest);
  return setOption.apply(this, args);
};

proto.resize = function (this: Chart, ...args: unknown[]) {
  const result = resize.apply(this, args);
  const state = historyStates.get(this);
  if (option !== 'current' && state && !this.isDisposed()) fitHistoryBars(this, state);
  return result;
};

proto.dispatchAction = function (this: Chart, ...args: unknown[]) {
  const action = args[0] as Loose | undefined;
  const state = option !== 'current' && isSiteChart(this) ? siteStates.get(this) : undefined;
  // Only the list's own actions: ECharts' axis pointer dispatches batched highlight / downplay itself.
  const fromList =
    (action?.type === 'highlight' && action.seriesId !== undefined && !action.batch) ||
    (action?.type === 'downplay' && action.seriesId === undefined && !action.batch);
  if (state && fromList) {
    // The list effect sends downplay and then highlight in one go; paint once, after both.
    state.legend = action!.type === 'highlight' ? String(action!.seriesId) : null;
    if (!scheduled.has(this)) {
      scheduled.add(this);
      queueMicrotask(() => {
        scheduled.delete(this);
        const latest = siteStates.get(this);
        if (latest && option !== 'current') siteFocus(this, latest);
      });
    }
    return;
  }
  return dispatchAction.apply(this, args);
};
