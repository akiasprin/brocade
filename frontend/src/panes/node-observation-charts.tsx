import { useEffect, useRef, useSyncExternalStore } from 'react';
import * as echarts from 'echarts/core';
import { CustomChart, LineChart } from 'echarts/charts';
import { GridComponent, MarkLineComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import type { PingProbeFamily, PingProbePoint } from '../api';
import { theme } from '../forge/theme';
import { palette } from '../forge/palette';
import {
  observeAreaStyle,
  observeAxisLine,
  observeAxisTick,
  observeColors,
  observeLoadingOptions,
  observeMsUnit,
  observeSeriesLine,
  observeTimeAxes,
  observeValueAxis,
} from '../ui/observe-chart';
import { PING_FAMILY_LABEL, pingLatencyMs, pingSampleLost, pingSampleText } from '../ui/ping-probe';
import { observeSampleStepSecs, observeSeriesData } from '../ui/observe-series';
import { echartsEntranceAnimation, useEchartsViewportEntry } from '../ui/echarts-motion';
import { OBSERVE_TOOLTIP_CLASS, ObserveBrushPlot, type ObserveBrushExtent } from '../ui/observe-brush';

echarts.use([LineChart, CustomChart, GridComponent, TooltipComponent, MarkLineComponent, CanvasRenderer]);

const NET_MONO = 'ui-monospace, SFMono-Regular, Menlo, monospace';

/** The same native ECharts progress surface used by expanded/deep metric charts. This lives in
 * the observation chunk so the machine-detail shell stays small; nodes.tsx paints a matching CSS
 * fallback only while this chunk itself is arriving. */
export function ObservationChartLoading({ className = 'node-observation-loading-chart' }: { className?: string }) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    chartRef.current = chart;
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
    const el = elRef.current;
    if (!chart || !el) return;
    const css = getComputedStyle(el);
    const resolve = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
    chart.showLoading('default', observeLoadingOptions(resolve));
  }, [paletteKey, themeName]);

  return <div ref={elRef} className={className} aria-label="加载中" />;
}

function html(value: string): string {
  return value.replace(
    /[&<>"']/g,
    char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]!,
  );
}

/** One curve of a Ping chart: one target's series for the family the panel shows. */
export interface PingChartLine {
  name: string;
  /** Position of the target in its block; the legend uses the same color. */
  color: number;
  samples: PingProbePoint[];
}

/** Sample spacing of the chart: the median gap between consecutive timestamps, so a missing report
 * or a changed interval does not widen every loss mark. */
function pingSampleStepMs(times: readonly number[]): number {
  return (observeSampleStepSecs(times) ?? 60) * 1000;
}

/** Periods in which any curve lost packets, as [start, end] milliseconds. A run of consecutive
 * losses is one period, each loss covers half a sample step on either side, and overlapping periods
 * of different curves are merged: the mark says when, the legend and tooltip say which target. */
export function pingLossPeriods(lines: readonly PingChartLine[], stepMs: number): Array<[number, number]> {
  const half = stepMs / 2;
  const periods: Array<[number, number]> = [];
  for (const line of lines) {
    const { samples } = line;
    for (let index = 0; index < samples.length; index += 1) {
      if (!pingSampleLost(samples[index])) continue;
      const start = index;
      while (
        index + 1 < samples.length &&
        pingSampleLost(samples[index + 1]) &&
        (samples[index + 1].probed_at_unix_secs - samples[index].probed_at_unix_secs) * 1000 <= stepMs * 1.5
      )
        index += 1;
      periods.push([
        samples[start].probed_at_unix_secs * 1000 - half,
        samples[index].probed_at_unix_secs * 1000 + half,
      ]);
    }
  }
  periods.sort((left, right) => left[0] - right[0]);
  const merged: Array<[number, number]> = [];
  for (const period of periods) {
    const last = merged.at(-1);
    if (last && period[0] <= last[1]) last[1] = Math.max(last[1], period[1]);
    else merged.push([...period]);
  }
  return merged;
}

/** Loss periods as a 3px lane just below the x axis. Curves only break where a reply is missing,
 * which a capability gap does too; the lane is what tells packet loss apart. It stays out of the
 * axis tooltip, which would otherwise snap to the period edges instead of to samples. */
function pingLossLane(periods: Array<[number, number]>, color: string): Record<string, unknown> {
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
      // A single loss in a week-long range is still a visible mark.
      const width = Math.max(2, right - left);
      const x = right - left < 2 ? (left + right) / 2 - 1 : left;
      return {
        type: 'rect',
        shape: { x, y: area.y + area.height + 2, width, height: 3, r: 1 },
        style: { fill: color, opacity: 0.8 },
      };
    },
  };
}

export function PingLatencyChart({
  lines,
  family,
  bounds,
  group,
}: {
  lines: PingChartLine[];
  family: PingProbeFamily;
  bounds: { startUnixSecs: number; endUnixSecs: number };
  group?: string;
}) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const enteredViewport = useEchartsViewportEntry(elRef);
  const hasRenderedData = useRef(false);
  const lastSig = useRef<string | null>(null);
  // 拖选按最近一次写入的坐标范围换算像素。
  const extentRef = useRef<ObserveBrushExtent | null>(null);

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    if (group) chart.group = group;
    chartRef.current = chart;
    lastSig.current = null;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(el);
    return () => {
      ro.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, [group]);

  useEffect(() => {
    const chart = chartRef.current;
    if (!chart || !enteredViewport) return;
    // An identical notMerge write would cancel the in-flight entrance sweep.
    const sig = JSON.stringify([lines, family, bounds.startUnixSecs, bounds.endUnixSecs, group, themeName, paletteKey]);
    if (sig === lastSig.current) return;
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const colors = observeColors(themeName, cv);
    const colorOf = (entry: PingChartLine) => colors[entry.color % colors.length];
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const err = cv('--err');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');
    const byLine = lines.map(entry => new Map(entry.samples.map(sample => [sample.probed_at_unix_secs, sample])));
    const times = [...new Set(lines.flatMap(entry => entry.samples.map(sample => sample.probed_at_unix_secs)))].sort(
      (left, right) => left - right,
    );
    const valuesAt = new Map(times.map(time => [time, byLine.map(samples => samples.get(time))] as const));
    const successful = lines.flatMap(entry =>
      entry.samples.flatMap(sample => {
        const latency = pingLatencyMs(sample);
        return latency == null ? [] : [latency];
      }),
    );
    const valueAxis = observeValueAxis(Math.max(...successful, 0.1));
    const start = bounds.startUnixSecs * 1000;
    const now = bounds.endUnixSecs * 1000;
    const firstMs = times.length > 0 ? times[0] * 1000 : start;
    const xMin = Math.min(firstMs, now - 30_000);
    const series: Array<Record<string, unknown>> = lines.map((entry, index) => {
      const color = colorOf(entry);
      return {
        name: entry.name,
        type: 'line' as const,
        symbol: 'circle',
        symbolSize: 5,
        showSymbol: false,
        smooth: false,
        connectNulls: false,
        lineStyle: observeSeriesLine(color),
        areaStyle: observeAreaStyle(color, themeName, { count: lines.length, paper: cv('--card') }),
        itemStyle: { color, borderColor: glass, borderWidth: 1.5 },
        emphasis: { disabled: true },
        data: observeSeriesData(
          times,
          times.map(time => {
            const sample = byLine[index].get(time);
            return sample ? pingLatencyMs(sample) : null;
          }),
        ),
      };
    });
    const lossPeriods = pingLossPeriods(lines, pingSampleStepMs(times));
    if (lossPeriods.length > 0) series.push(pingLossLane(lossPeriods, err));
    chart.setOption(
      {
        ...echartsEntranceAnimation(!hasRenderedData.current),
        color: colors,
        grid: { left: 10, right: 14, top: 12, bottom: 10, containLabel: true },
        textStyle: { fontFamily: NET_MONO },
        tooltip: {
          trigger: 'axis',
          confine: true,
          className: OBSERVE_TOOLTIP_CLASS,
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: NET_MONO },
          extraCssText: 'border-radius:8px; box-shadow:0 8px 24px rgba(0,0,0,.18); backdrop-filter:blur(8px);',
          axisPointer: { type: 'line', lineStyle: { color: ink4, width: 1, type: 'dashed' }, z: 0 },
          formatter: (params: unknown) => {
            const entries = params as { axisValue: number }[];
            const atMs = Number(entries[0]?.axisValue ?? 0);
            const at = Math.round(atMs / 1000);
            const values = valuesAt.get(at) ?? lines.map(() => undefined);
            const when = new Date(atMs).toLocaleString('zh-CN', {
              month: '2-digit',
              day: '2-digit',
              hour: '2-digit',
              minute: '2-digit',
              second: '2-digit',
              hour12: false,
            });
            const rows = lines
              .map((entry, index) => ({
                entry,
                sample: values[index],
                value: values[index] ? pingLatencyMs(values[index]!) : null,
              }))
              .sort(
                (left, right) => (right.value ?? Number.NEGATIVE_INFINITY) - (left.value ?? Number.NEGATIVE_INFINITY),
              )
              .map(({ entry, sample }) => {
                const tone = pingSampleLost(sample) ? err : sample && !sample.attempted ? ink4 : ink;
                return (
                  `<div style="display:flex;align-items:center;gap:7px;line-height:1.75">` +
                  `<span style="width:8px;height:8px;border-radius:2px;background:${colorOf(entry)};flex:none"></span>` +
                  `<span style="color:${ink3}">${html(entry.name)}</span>` +
                  `<b style="margin-left:auto;color:${tone};font-weight:500">${pingSampleText(sample)}</b></div>`
                );
              })
              .join('');
            return `<div style="color:${ink4};font-size:9px;margin-bottom:4px;letter-spacing:.04em">${when} · ${PING_FAMILY_LABEL[family]}</div>${rows}`;
          },
        },
        xAxis: observeTimeAxes(xMin, now, ink3, lineSoft),
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
    extentRef.current = { minMs: xMin, maxMs: now, yMax: valueAxis.max };
    hasRenderedData.current = true;
    lastSig.current = sig;
    if (group) echarts.connect(group);
  }, [lines, family, bounds, group, themeName, paletteKey, enteredViewport]);

  return <ObserveBrushPlot className="ping-probe-chart" hostRef={elRef} chartRef={chartRef} extentRef={extentRef} />;
}
