import { useEffect, useRef, useSyncExternalStore } from 'react';
import * as echarts from 'echarts/core';
import { CustomChart, LineChart } from 'echarts/charts';
import { GridComponent, MarkAreaComponent, MarkLineComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import type { TunnelProbeView } from './api';
import { palette } from './forge/palette';
import { theme } from './forge/theme';
import { echartsEntranceAnimation, useEchartsViewportEntry } from './ui/echarts-motion';
import { OBSERVE_TOOLTIP_CLASS } from './ui/observe-brush';
import {
  observeAreaStyle,
  observeAxisLine,
  observeAxisTick,
  observeColors,
  observeMsUnit,
  observeSeriesLine,
  observeTimeAxes,
  observeValueAxis,
} from './ui/observe-chart';
import { observeSeriesData } from './ui/observe-series';

// The tunnel detail loads this module on demand, so the tunnel list never downloads ECharts.
echarts.use([
  LineChart,
  CustomChart,
  GridComponent,
  TooltipComponent,
  MarkLineComponent,
  MarkAreaComponent,
  CanvasRenderer,
]);

export type TunnelProbeChartPoint = TunnelProbeView['points'][number];

const MONO = 'ui-monospace, SFMono-Regular, Menlo, monospace';
const FAILURE_TEXT: Record<TunnelProbeChartPoint['result'], string> = {
  ok: '成功',
  timeout: '超时',
  'connect-failed': '连接失败',
  'target-failed': '落点失败',
  unsupported: '不支持',
  canceled: '已取消',
  interrupted: '执行中断',
};

const two = (value: number) => String(value).padStart(2, '0');
const clock = (ms: number) => {
  const date = new Date(ms);
  const time = `${two(date.getHours())}:${two(date.getMinutes())}`;
  return date.toDateString() === new Date().toDateString()
    ? time
    : `${two(date.getMonth() + 1)}-${two(date.getDate())} ${time}`;
};

/** Longest spacing between the runs of two neighbouring buckets when no scheduled run is missing:
 * one bucket plus one probe interval. Anything longer is a period without runs. */
export const tunnelProbeGapSecs = (bucketSecs: number, intervalSecs: number | null) => {
  const cadence = intervalSecs ?? bucketSecs;
  return Math.max(bucketSecs, cadence) + Math.min(bucketSecs, cadence);
};

/** Failed runs as [start, end] milliseconds, half a bucket on either side, overlaps merged.
 * The same reading as the Ping chart's loss lane: the curve breaks where a run failed, and the lane
 * under the axis tells a failure apart from a period without any run. */
export function tunnelProbeFailurePeriods(
  points: readonly TunnelProbeChartPoint[],
  bucketSecs: number,
): Array<[number, number]> {
  const half = (bucketSecs * 1000) / 2;
  const merged: Array<[number, number]> = [];
  for (const point of points) {
    if (point.result === 'ok') continue;
    const at = point.finished_at_unix_secs * 1000;
    const last = merged.at(-1);
    if (last && at - half <= last[1]) last[1] = Math.max(last[1], at + half);
    else merged.push([at - half, at + half]);
  }
  return merged;
}

function failureLane(periods: Array<[number, number]>, color: string): Record<string, unknown> {
  type Coord = { x: number; y: number; width: number; height: number };
  return {
    type: 'custom',
    silent: true,
    animation: false,
    clip: false,
    z: 6,
    tooltip: { show: false },
    data: periods,
    encode: { x: [0, 1] },
    renderItem: (
      params: { coordSys: unknown },
      api: { value: (dimension: number) => number; coord: (value: number[]) => number[] },
    ) => {
      const area = params.coordSys as Coord;
      const left = Math.max(area.x, api.coord([api.value(0), 0])[0]);
      const right = Math.min(area.x + area.width, api.coord([api.value(1), 0])[0]);
      // One failed run in a week-long window is still a visible mark.
      const width = Math.max(2, right - left);
      const x = right - left < 2 ? (left + right) / 2 - 1 : left;
      return {
        type: 'rect',
        shape: { x, y: area.y + area.height + 2, width, height: 3, r: 1 },
        style: { fill: color, opacity: 0.85 },
      };
    },
  };
}

/** First-byte time of each sampled Serving run across the selected window. */
export function TunnelProbeChart({
  points,
  windowSecs,
  endUnixSecs,
  bucketSecs,
  intervalSecs,
  p50Ms,
  slowMs,
}: {
  points: readonly TunnelProbeChartPoint[];
  windowSecs: number;
  endUnixSecs: number;
  bucketSecs: number;
  /** The scheduled cadence; null when only manual runs exist. */
  intervalSecs: number | null;
  p50Ms: number | null;
  slowMs: number;
}) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const enteredViewport = useEchartsViewportEntry(elRef);
  const hasRenderedData = useRef(false);
  const lastSig = useRef<string | null>(null);

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    chartRef.current = chart;
    lastSig.current = null;
    const observer = new ResizeObserver(() => chart.resize());
    observer.observe(el);
    return () => {
      observer.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, []);

  useEffect(() => {
    const chart = chartRef.current;
    if (!chart || !enteredViewport) return;
    // An identical notMerge write would cancel the in-flight entrance sweep.
    const sig = JSON.stringify([
      points,
      windowSecs,
      endUnixSecs,
      bucketSecs,
      intervalSecs,
      p50Ms,
      slowMs,
      themeName,
      paletteKey,
    ]);
    if (sig === lastSig.current) return;
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const color = observeColors(themeName, cv)[0];
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const err = cv('--err');
    const gold = cv('--gold');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');

    const endMs = endUnixSecs * 1000;
    const startMs = endMs - windowSecs * 1000;
    const times = points.map(point => point.finished_at_unix_secs);
    const gapSecs = tunnelProbeGapSecs(bucketSecs, intervalSecs);
    const values = points.map(point => (point.result === 'ok' ? point.ttfb_ms : null));
    const peak = Math.max(slowMs * 1.05, ...values.map(value => value ?? 0));
    const valueAxis = observeValueAxis(peak);
    const axes = observeTimeAxes(startMs, endMs, ink3, lineSoft);
    // Without any run before it, the left part of the window is unobserved, not zero.
    const firstMs = points.length > 0 ? points[0].finished_at_unix_secs * 1000 : endMs;
    const unobserved = firstMs - startMs > gapSecs * 1000;
    const markLines: Array<Record<string, unknown> | Array<Record<string, unknown>>> = [
      {
        yAxis: slowMs,
        lineStyle: { color: gold, opacity: 0.7, type: [4, 4], width: 1 },
        label: { position: 'insideEndTop', color: gold, fontSize: 9.5, fontFamily: MONO, formatter: `${slowMs} 变慢` },
      },
    ];
    if (p50Ms != null && points.length > 0) {
      // The median describes the observed runs only, so its line starts at the first of them.
      markLines.push([
        {
          coord: [firstMs, p50Ms],
          lineStyle: { color: ink3, opacity: 0.8, type: [2, 3], width: 1 },
          label: {
            position: 'insideEndBottom',
            color: ink3,
            fontSize: 9.5,
            fontFamily: MONO,
            formatter: `P50 ${p50Ms}`,
          },
        },
        { coord: [endMs, p50Ms] },
      ]);
    }
    const series: Array<Record<string, unknown>> = [
      {
        name: '首字节',
        type: 'line',
        symbol: 'circle',
        symbolSize: 5,
        // A handful of runs reads as points; a full window reads as a curve.
        showSymbol: points.length <= 40,
        smooth: false,
        connectNulls: false,
        lineStyle: observeSeriesLine(color),
        areaStyle: observeAreaStyle(color, themeName, { count: 1, paper: cv('--card') }),
        itemStyle: { color, borderColor: glass, borderWidth: 1.5 },
        emphasis: { disabled: true },
        // observeSeriesData breaks the line past 1.5 steps; one step is two thirds of the gap.
        data: observeSeriesData(times, values, { sampleStepSecs: gapSecs / 1.5 }),
        markLine: { silent: true, symbol: 'none', animation: false, data: markLines },
        markArea: unobserved
          ? {
              silent: true,
              animation: false,
              itemStyle: { color: ink, opacity: 0.04 },
              label: {
                position: 'inside',
                color: ink4,
                fontSize: 11,
                formatter: points.length > 0 ? `${clock(firstMs)} 之前没有拨测` : '这段时间没有 Serving 拨测',
              },
              data: [[{ xAxis: startMs }, { xAxis: firstMs }]],
            }
          : undefined,
      },
    ];
    const failures = tunnelProbeFailurePeriods(points, bucketSecs);
    if (failures.length > 0) series.push(failureLane(failures, err));

    chart.setOption(
      {
        ...echartsEntranceAnimation(!hasRenderedData.current),
        grid: { left: 10, right: 14, top: 14, bottom: 10, containLabel: true },
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
            const atMs = Number((params as { axisValue: number }[])[0]?.axisValue ?? 0);
            // Gap separators sit between runs; the reading belongs to the nearest real run.
            const nearest = points.reduce<TunnelProbeChartPoint | null>(
              (best, point) =>
                !best ||
                Math.abs(point.finished_at_unix_secs * 1000 - atMs) < Math.abs(best.finished_at_unix_secs * 1000 - atMs)
                  ? point
                  : best,
              null,
            );
            if (!nearest) return '';
            const reading =
              nearest.result === 'ok'
                ? `<b style="color:${ink};font-weight:600">${nearest.ttfb_ms} ms</b>`
                : `<b style="color:${err};font-weight:600">${FAILURE_TEXT[nearest.result]}</b>`;
            return `<div style="color:${ink4};font-size:9px;margin-bottom:3px;letter-spacing:.04em">${clock(nearest.finished_at_unix_secs * 1000)} · 首字节</div>${reading}`;
          },
        },
        xAxis: axes,
        yAxis: {
          type: 'value',
          min: 0,
          max: valueAxis.max,
          interval: valueAxis.interval,
          axisLabel: { color: ink3, fontSize: 9.5, margin: 8, formatter: observeMsUnit(valueAxis.interval).text },
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
        },
        series,
      },
      true,
    );
    hasRenderedData.current = true;
    lastSig.current = sig;
  }, [
    points,
    windowSecs,
    endUnixSecs,
    bucketSecs,
    intervalSecs,
    p50Ms,
    slowMs,
    themeName,
    paletteKey,
    enteredViewport,
  ]);

  return <div ref={elRef} className="tnd-chart" role="img" aria-label="首字节走势" />;
}
