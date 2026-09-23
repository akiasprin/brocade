import { useEffect, useRef, useSyncExternalStore } from 'react';
import * as echarts from 'echarts/core';
import { LineChart } from 'echarts/charts';
import { GridComponent, MarkLineComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import type { NodePingProbeView } from '../api';
import { theme } from '../forge/theme';
import { palette } from '../forge/palette';
import {
  observeAreaStyle,
  observeAxisLine,
  observeAxisTick,
  observeBpsUnit,
  observeColors,
  observeLoadingOptions,
  observeMinorTick,
  observeMsUnit,
  observeSeriesLine,
  observeTimeInterval,
  observeValueAxis,
} from '../ui/observe-chart';
import { pingLatencyMs, pingSampleText } from '../ui/ping-probe';
import { echartsEntranceAnimation, useEchartsViewportEntry } from '../ui/echarts-motion';

echarts.use([LineChart, GridComponent, TooltipComponent, MarkLineComponent, CanvasRenderer]);

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

export interface FleetNetPoint {
  nicRx: number;
  nicTx: number;
  xrayRx: number;
  xrayTx: number;
}

/* 镜像面积图。接收为正（朝上）、发送取负（朝下），零线居中；每方向 NIC 外层（淡填充 +
   描边）套 XRAY 内层（实心），两线之间的缝即封装 / 系统开销。方向由镜像位置表达，用色
   同一数据色的两档（接收=data、发送=data-secondary），来源用填充手法区分。颜色与坐标
   全部读自 CSS 令牌，随明暗主题与调色盘选择重绘。 */
export function FleetNetChart({ times, pts, peak }: { times: number[]; pts: FleetNetPoint[]; peak: number }) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const enteredViewport = useEchartsViewportEntry(elRef);
  const hasRenderedData = useRef(false);

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    chartRef.current = chart;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(el);
    return () => {
      ro.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, []);

  useEffect(() => {
    const chart = chartRef.current;
    if (!chart || !enteredViewport) return;
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const data = cv('--data');
    const dataSecondary = cv('--data-secondary');
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');
    const valueAxis = observeValueAxis(peak);
    const unit = observeBpsUnit(valueAxis);

    const area = (name: string, key: keyof FleetNetPoint, color: string, sign: 1 | -1, inner: boolean) => ({
      name,
      type: 'line' as const,
      showSymbol: false,
      smooth: true,
      sampling: 'lttb' as const,
      lineStyle: { color, width: inner ? 1 : 1.3, opacity: inner ? 1 : 0.9 },
      areaStyle: { color, opacity: inner ? 0.42 : 0.12 },
      emphasis: { disabled: true },
      z: inner ? 3 : 2,
      data: times.map((time, index) => [time * 1000, sign * pts[index][key]] as [number, number]),
    });

    const series = [
      area('接收 · 网卡', 'nicRx', data, 1, false),
      area('接收 · 承载', 'xrayRx', data, 1, true),
      area('发送 · 网卡', 'nicTx', dataSecondary, -1, false),
      area('发送 · 承载', 'xrayTx', dataSecondary, -1, true),
    ];
    (series[0] as Record<string, unknown>).markLine = {
      silent: true,
      symbol: 'none',
      data: [{ yAxis: 0 }],
      lineStyle: { color: ink4, width: 0.8, opacity: 0.55 },
      label: { show: false },
    };

    chart.setOption(
      {
        ...echartsEntranceAnimation(!hasRenderedData.current),
        grid: { left: 48, right: 12, top: 10, bottom: 20 },
        textStyle: { fontFamily: NET_MONO },
        tooltip: {
          trigger: 'axis',
          backgroundColor: glass,
          borderColor: line,
          borderWidth: 1,
          padding: [7, 9],
          textStyle: { color: ink3, fontSize: 11, fontFamily: NET_MONO },
          formatter: (params: unknown) => {
            const entries = params as { seriesName: string; color: string; value: [number, number] }[];
            const when = new Date(entries[0].value[0]).toLocaleTimeString('zh-CN', {
              hour: '2-digit',
              minute: '2-digit',
            });
            const row = (entry: { seriesName: string; color: string; value: [number, number] }) =>
              `<div style="display:flex;gap:8px;align-items:center;line-height:1.75">` +
              `<span style="width:8px;height:8px;border-radius:2px;background:${entry.color}"></span>` +
              `<span>${entry.seriesName}</span>` +
              `<b style="margin-left:auto;color:${ink}">${unit.read(Math.abs(entry.value[1]))}</b></div>`;
            return `<div style="color:${ink4};font-size:9px;margin-bottom:3px">${when}</div>${entries.map(row).join('')}`;
          },
        },
        xAxis: {
          type: 'time',
          axisLabel: { color: ink4, fontSize: 9, hideOverlap: true },
          axisLine: { ...observeAxisLine(ink3), onZero: false },
          axisTick: observeAxisTick(ink3),
          minorTick: observeMinorTick(lineSoft),
          splitLine: { show: false },
        },
        yAxis: {
          type: 'value',
          min: -valueAxis.max,
          max: valueAxis.max,
          interval: valueAxis.interval,
          axisLabel: { color: ink4, fontSize: 9, formatter: (value: number) => unit.text(Math.abs(value)) },
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          splitLine: { lineStyle: { color: lineSoft } },
        },
        series,
      },
      true,
    );
    hasRenderedData.current = true;
  }, [enteredViewport, times, pts, peak, themeName, paletteKey]);

  return <div ref={elRef} className="ndnet-echart" />;
}

function html(value: string): string {
  return value.replace(
    /[&<>"']/g,
    char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]!,
  );
}

export function PingLatencyChart({
  view,
  bounds,
  group,
}: {
  view: NodePingProbeView;
  bounds: { startUnixSecs: number; endUnixSecs: number };
  group?: string;
}) {
  const elRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<ReturnType<typeof echarts.init> | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteKey = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const enteredViewport = useEchartsViewportEntry(elRef);
  const hasRenderedData = useRef(false);

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    const chart = echarts.init(el, null, { renderer: 'canvas' });
    if (group) chart.group = group;
    chartRef.current = chart;
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
    const css = getComputedStyle(document.documentElement);
    const cv = (name: string) => css.getPropertyValue(name).trim();
    const colors = observeColors(themeName, cv);
    const ink = cv('--ink');
    const ink3 = cv('--ink-3');
    const ink4 = cv('--ink-4');
    const line = cv('--line');
    const lineSoft = cv('--line-soft');
    const glass = cv('--glass-strong');
    const byTarget = view.targets.map(
      target => new Map(target.samples.map(sample => [sample.probed_at_unix_secs, sample])),
    );
    const times = [
      ...new Set(view.targets.flatMap(target => target.samples.map(sample => sample.probed_at_unix_secs))),
    ].sort((left, right) => left - right);
    const valuesAt = new Map(times.map(time => [time, byTarget.map(samples => samples.get(time))] as const));
    const successful = view.targets.flatMap(target =>
      target.samples.flatMap(sample => {
        const latency = pingLatencyMs(sample);
        return latency == null ? [] : [latency];
      }),
    );
    const valueAxis = observeValueAxis(Math.max(...successful, 0.1));
    const start = bounds.startUnixSecs * 1000;
    const now = bounds.endUnixSecs * 1000;
    const firstMs = times.length > 0 ? times[0] * 1000 : start;
    const xStep = observeTimeInterval(now - firstMs);
    const xMin = Math.min(firstMs, now - 30_000);
    const hm = (milliseconds: number) =>
      new Date(milliseconds).toLocaleTimeString('zh-CN', {
        hour: '2-digit',
        minute: '2-digit',
        hour12: false,
      });
    const series: Array<Record<string, unknown>> = view.targets.map((target, index) => {
      const color = colors[index % colors.length];
      return {
        name: target.name,
        type: 'line' as const,
        symbol: 'circle',
        symbolSize: 5,
        showSymbol: false,
        smooth: false,
        connectNulls: false,
        lineStyle: observeSeriesLine(color),
        areaStyle: observeAreaStyle(color, themeName, { count: view.targets.length }),
        itemStyle: { color, borderColor: glass, borderWidth: 1.5 },
        emphasis: { disabled: true },
        data: times.map(time => {
          const sample = byTarget[index].get(time);
          return [time * 1000, sample ? pingLatencyMs(sample) : null] as [number, number | null];
        }),
      };
    });
    chart.setOption(
      {
        ...echartsEntranceAnimation(!hasRenderedData.current),
        color: colors,
        grid: { left: 10, right: 14, top: 12, bottom: 10, containLabel: true },
        textStyle: { fontFamily: NET_MONO },
        tooltip: {
          trigger: 'axis',
          confine: true,
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
            const values = valuesAt.get(at) ?? view.targets.map(() => undefined);
            const when = new Date(atMs).toLocaleString('zh-CN', {
              month: '2-digit',
              day: '2-digit',
              hour: '2-digit',
              minute: '2-digit',
              second: '2-digit',
              hour12: false,
            });
            const rows = view.targets
              .map((target, index) => ({
                target,
                index,
                sample: values[index],
                value: values[index] ? pingLatencyMs(values[index]!) : null,
              }))
              .sort(
                (left, right) => (right.value ?? Number.NEGATIVE_INFINITY) - (left.value ?? Number.NEGATIVE_INFINITY),
              )
              .map(
                ({ target, index, sample }) =>
                  `<div style="display:flex;align-items:center;gap:7px;line-height:1.75">` +
                  `<span style="width:8px;height:8px;border-radius:2px;background:${colors[index % colors.length]};flex:none"></span>` +
                  `<span style="color:${ink3}">${html(target.name)}</span>` +
                  `<b style="margin-left:auto;color:${ink};font-weight:500">${pingSampleText(sample)}</b></div>`,
              )
              .join('');
            return `<div style="color:${ink4};font-size:9px;margin-bottom:4px;letter-spacing:.04em">${when}</div>${rows}`;
          },
        },
        xAxis: {
          type: 'value',
          min: xMin,
          max: now,
          interval: xStep,
          axisLabel: {
            color: ink3,
            fontSize: 9.5,
            margin: 8,
            hideOverlap: true,
            formatter: (value: number) => hm(value),
          },
          axisLine: observeAxisLine(ink3),
          axisTick: observeAxisTick(ink3),
          minorTick: observeMinorTick(lineSoft),
          splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
        },
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
    if (group) echarts.connect(group);
  }, [view, bounds, group, themeName, paletteKey, enteredViewport]);

  return <div ref={elRef} className="ping-probe-chart" />;
}
