import { useEffect, useRef, useSyncExternalStore } from 'react';
import * as echarts from 'echarts/core';
import { BarChart } from 'echarts/charts';
import { GridComponent, MarkAreaComponent, MarkLineComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import { palette } from './forge/palette';
import { theme } from './forge/theme';
import { echartsEntranceAnimation, useEchartsViewportEntry } from './ui/echarts-motion';
import { OBSERVE_TOOLTIP_CLASS } from './ui/observe-brush';
import {
  observeAxisLine,
  observeAxisTick,
  observeTimeInterval,
  observeValueAxis,
  observeWallClockTicks,
} from './ui/observe-chart';
import { bytes } from './ui/format';
import type { SiteUsagePeriod } from './api';

// The user detail loads this module on demand, so pages without site reports never download ECharts.
echarts.use([BarChart, GridComponent, TooltipComponent, MarkAreaComponent, MarkLineComponent, CanvasRenderer]);

/** A color token and an optional opacity; the HTML swatch beside the chart uses the same pair. */
export interface SiteUsageColor {
  token: string;
  alpha?: number;
}

export interface SiteUsageChartSeries {
  key: string;
  name: string;
  color: SiteUsageColor;
  /** Bytes per bucket; null for a bucket of the period that has not started yet. */
  values: (number | null)[];
}

const MONO = 'ui-monospace, SFMono-Regular, Menlo, monospace';
/** Calendar periods are +08 (see the site usage store); labels use the same wall clock. */
const ZONE_SECS = 8 * 3600;
const WEEK = ['日', '一', '二', '三', '四', '五', '六'];
const two = (value: number) => String(value).padStart(2, '0');
const wall = (unixSecs: number) => new Date((unixSecs + ZONE_SECS) * 1000);
const hm = (unixSecs: number) => `${two(wall(unixSecs).getUTCHours())}:${two(wall(unixSecs).getUTCMinutes())}`;
const md = (unixSecs: number, withYear = false) => {
  const date = wall(unixSecs);
  return `${withYear ? `${date.getUTCFullYear()}-` : ''}${two(date.getUTCMonth() + 1)}-${two(date.getUTCDate())}`;
};
const weekday = (unixSecs: number) => `周${WEEK[wall(unixSecs).getUTCDay()]}`;

/** Bucket indexes that carry an axis label: every six hours, every day, the 1st and every fifth day. */
function labelled(period: SiteUsagePeriod, start: number, step: number, count: number): number[] {
  const marks: number[] = [];
  for (let index = 0; index < count; index++) {
    const at = wall(start + index * step);
    if (period === 'day' && at.getUTCHours() % 6 === 0) marks.push(index);
    if (period === 'week' && at.getUTCHours() === 0) marks.push(index);
    if (period === 'month' && (at.getUTCDate() === 1 || at.getUTCDate() % 5 === 0)) marks.push(index);
  }
  return marks;
}

function labelText(period: SiteUsagePeriod, unixSecs: number): string {
  if (period === 'day') return hm(unixSecs);
  if (period === 'week') return `${md(unixSecs)} ${weekday(unixSecs)}`;
  return md(unixSecs);
}

const BYTE_RUNGS: readonly (readonly [number, string])[] = [
  [1024 ** 4, 'TiB'],
  [1024 ** 3, 'GiB'],
  [1024 ** 2, 'MiB'],
  [1024, 'KiB'],
  [1, 'B'],
];

/** Byte axis whose steps are round in the displayed unit: the unit comes from the peak first, so a
 * 1.2 GiB peak reads 0.25 / 0.5 … GiB rather than decimal byte steps shown as 0.23 / 0.47 GiB. */
function bytesAxis(peak: number) {
  const [divisor, name] = BYTE_RUNGS.find(([size]) => peak / size >= 1) ?? BYTE_RUNGS[BYTE_RUNGS.length - 1];
  const scaled = observeValueAxis(peak / divisor);
  return {
    max: scaled.max * divisor,
    interval: scaled.interval * divisor,
    label: (value: number) => (value === 0 ? '0' : `${Number((value / divisor).toFixed(2))} ${name}`),
  };
}

function rgba(color: string, alpha: number | undefined): string {
  if (alpha === undefined) return color;
  const hex = /^#([0-9a-f]{6})$/i.exec(color.trim());
  if (!hex) return color;
  const value = Number.parseInt(hex[1], 16);
  return `rgba(${(value >> 16) & 255}, ${(value >> 8) & 255}, ${value & 255}, ${alpha})`;
}

/** Stacked traffic per bucket of a calendar period. The x axis spans the whole period; buckets still
 * to come stay empty, and a line marks the current time. */
export default function SiteUsageChart({
  period,
  startUnixSecs,
  bucketSecs,
  nowUnixSecs,
  observedSinceUnixSecs,
  uncertainBeforeUnixSecs = null,
  series,
  focusKey,
  customRange = false,
}: {
  period: SiteUsagePeriod;
  startUnixSecs: number;
  bucketSecs: number;
  nowUnixSecs: number;
  /** Later than the period start when observation began inside the period. */
  observedSinceUnixSecs: number | null;
  /** A recent continuous baseline says nothing about where earlier gaps occurred. */
  uncertainBeforeUnixSecs?: number | null;
  /** Bottom to top of the stack. */
  series: SiteUsageChartSeries[];
  /** The series the list beside the chart points at; the others fade. */
  focusKey: string | null;
  /** A fixed range uses its actual bounds and cadence, independently of the period query key. */
  customRange?: boolean;
}) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const enteredViewport = useEchartsViewportEntry(elRef);
  const hasRenderedData = useRef(false);
  const lastSig = useRef<string | null>(null);
  const fitObservationLabel = useRef<(() => void) | null>(null);

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    chartRef.current = chart;
    lastSig.current = null;
    const observer = new ResizeObserver(() => {
      chart.resize();
      fitObservationLabel.current?.();
    });
    observer.observe(el);
    return () => {
      observer.disconnect();
      chart.dispose();
      chartRef.current = null;
      fitObservationLabel.current = null;
    };
  }, []);

  useEffect(() => {
    const chart = chartRef.current;
    if (!chart || !enteredViewport) return;
    // An identical notMerge write would cancel the entrance sweep and drop a hovered tooltip.
    const sig = JSON.stringify([
      period,
      customRange,
      startUnixSecs,
      bucketSecs,
      nowUnixSecs,
      observedSinceUnixSecs,
      uncertainBeforeUnixSecs,
      series,
      themeName,
      paletteKey,
    ]);
    if (sig === lastSig.current) return;
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');

    const count = series[0]?.values.length ?? 0;
    const totals = Array.from({ length: count }, (_, index) =>
      series.reduce((sum, item) => sum + (item.values[index] ?? 0), 0),
    );
    const valueAxis = bytesAxis(Math.max(...totals, 0));
    const endUnixSecs = startUnixSecs + count * bucketSecs;
    const tickStep = Math.max(bucketSecs * 1000, observeTimeInterval(count * bucketSecs * 1000));
    const marks = customRange
      ? observeWallClockTicks(startUnixSecs * 1000, endUnixSecs * 1000, tickStep, ZONE_SECS / 60).map(
          value => (value / 1000 - startUnixSecs) / bucketSecs,
        )
      : labelled(period, startUnixSecs, bucketSecs, count);
    const withYear = wall(startUnixSecs).getUTCFullYear() !== wall(endUnixSecs).getUTCFullYear();
    const withDate = withYear || md(startUnixSecs) !== md(endUnixSecs);
    const customClock = (at: number) => `${withDate ? `${md(at, withYear)} ` : ''}${hm(at)}`;
    const nowAt = (nowUnixSecs - startUnixSecs) / bucketSecs;
    const unobservedTo =
      observedSinceUnixSecs !== null ? Math.min(count, (observedSinceUnixSecs - startUnixSecs) / bucketSecs) : 0;
    const colors = series.map(item => rgba(cv(item.color.token), item.color.alpha));

    const bars = series.map((item, index) => ({
      id: item.key,
      name: item.name,
      type: 'bar' as const,
      stack: 'sites',
      barWidth: '72%',
      itemStyle: { color: colors[index] },
      emphasis: { focus: 'series' as const, blurScope: 'coordinateSystem' as const },
      // Bars sit at bucket centres on a value axis, so the current-time line can fall inside a bucket.
      data: item.values.map((value, bucket) => [bucket + 0.5, value]),
      ...(index === 0
        ? {
            markLine:
              !customRange && nowAt < count
                ? {
                    silent: true,
                    symbol: 'none',
                    animation: false,
                    lineStyle: { color: ink4, width: 1, type: 'solid' },
                    label: { position: 'end', distance: 3, color: ink4, fontSize: 9.5, formatter: '现在' },
                    data: [{ xAxis: nowAt }],
                  }
                : undefined,
            markArea:
              unobservedTo > 0
                ? {
                    silent: true,
                    animation: false,
                    itemStyle: { color: ink, opacity: 0.04 },
                    label: { show: false, position: 'insideTopLeft', color: ink4, fontSize: 10.5, formatter: '未观测' },
                    data: [[{ xAxis: 0 }, { xAxis: unobservedTo }]],
                  }
                : undefined,
          }
        : {}),
    }));

    chart.setOption(
      {
        ...echartsEntranceAnimation(!hasRenderedData.current),
        // The top margin leaves room for the 现在 label above the current-time line.
        grid: { left: 10, right: 12, top: 20, bottom: 6, containLabel: true },
        textStyle: { fontFamily: MONO },
        tooltip: {
          trigger: 'axis',
          confine: true,
          className: OBSERVE_TOOLTIP_CLASS,
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: MONO },
          extraCssText: 'border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.18); backdrop-filter:blur(8px);',
          axisPointer: { type: 'line', lineStyle: { color: ink4, width: 1, type: 'dashed' }, z: 0 },
          formatter: (params: unknown) => {
            const list = params as { seriesIndex: number; value: [number, number | null] }[];
            const bucket = Math.floor(list[0]?.value[0] ?? 0);
            const from = startUnixSecs + bucket * bucketSecs;
            if (from >= nowUnixSecs) return '';
            const to = Math.min(from + bucketSecs, nowUnixSecs);
            const until = to === nowUnixSecs ? '现在' : hm(to);
            const head = customRange
              ? `${customClock(from)} – ${customClock(to)}`
              : period === 'month'
                ? `${md(from)} ${weekday(from)}${to === nowUnixSecs ? ' · 截至现在' : ''}`
                : `${period === 'week' ? `${md(from)} ${weekday(from)} ` : ''}${hm(from)} – ${until}`;
            const rows = [...list]
              .reverse()
              .filter(item => (item.value[1] ?? 0) > 0)
              .sort(
                (a, b) =>
                  Number(series[b.seriesIndex].key === 'forwarding') -
                  Number(series[a.seriesIndex].key === 'forwarding'),
              )
              .map(
                item =>
                  `<div style="display:flex;gap:10px;justify-content:space-between"><span><i style="display:inline-block;width:7px;height:7px;border-radius:2px;margin-right:6px;background:${colors[item.seriesIndex]}"></i>${series[item.seriesIndex].name}</span><b style="color:${ink};font-weight:500">${bytes(item.value[1] ?? 0)}</b></div>`,
              )
              .join('');
            const empty =
              bucket + 1 <= unobservedTo
                ? '未观测'
                : uncertainBeforeUnixSecs !== null && startUnixSecs + bucket * bucketSecs < uncertainBeforeUnixSecs
                  ? '无记录 · 采集可能不完整'
                  : '无流量';
            return `<div style="color:${ink4};font-size:9.5px;margin-bottom:4px">${head}</div>${rows || empty}`;
          },
        },
        xAxis: {
          type: 'value',
          min: 0,
          max: count,
          axisLine: observeAxisLine(ink3),
          axisTick: { ...observeAxisTick(ink3), customValues: marks },
          splitLine: { show: false },
          axisLabel: {
            color: ink3,
            fontSize: 9.5,
            margin: 8,
            hideOverlap: true,
            customValues: marks,
            formatter: (value: number) => {
              const at = startUnixSecs + Math.round(value) * bucketSecs;
              if (!customRange) return labelText(period, at);
              if (!withDate) return hm(at);
              const date = md(at, withYear);
              return tickStep >= 86_400_000 ? date : `${date}\n${hm(at)}`;
            },
          },
          axisPointer: { snap: true },
        },
        yAxis: {
          type: 'value',
          min: 0,
          max: valueAxis.max,
          interval: valueAxis.interval,
          axisLabel: {
            color: ink3,
            fontSize: 9.5,
            margin: 8,
            formatter: valueAxis.label,
          },
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
        },
        series: bars,
      },
      true,
    );
    // Measure the actual plotting area after axis layout, including when the card is resized.
    // Keep the shading even when the three-character label would run into the first bar.
    let labelShown = false;
    fitObservationLabel.current = () => {
      if (unobservedTo <= 0 || !series[0]) return;
      const width = chart.convertToPixel({ xAxisIndex: 0 }, unobservedTo) - chart.convertToPixel({ xAxisIndex: 0 }, 0);
      const show = Number.isFinite(width) && width >= 48;
      if (show === labelShown) return;
      labelShown = show;
      chart.setOption({ series: [{ id: series[0].key, markArea: { label: { show } } }] });
    };
    fitObservationLabel.current();
    hasRenderedData.current = true;
    lastSig.current = sig;
  }, [
    period,
    customRange,
    startUnixSecs,
    bucketSecs,
    nowUnixSecs,
    observedSinceUnixSecs,
    uncertainBeforeUnixSecs,
    series,
    themeName,
    paletteKey,
    enteredViewport,
  ]);

  // Pointing at a row of the list beside the chart fades every other category.
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart || !enteredViewport) return;
    chart.dispatchAction({ type: 'downplay' });
    if (focusKey) chart.dispatchAction({ type: 'highlight', seriesId: focusKey });
  }, [focusKey, enteredViewport, series]);

  return <div ref={elRef} className="site-usage-chart" role="img" aria-label="站点流量随时间分布" />;
}
