import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import * as echarts from 'echarts/core';
import { CustomChart, LineChart, ScatterChart } from 'echarts/charts';
import { GridComponent, MarkAreaComponent, MarkLineComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
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
import {
  tunnelProbeBuckets,
  tunnelProbeMissingPeriods,
  tunnelProbeFailurePeriods,
  tunnelProbeLineData,
  type TunnelProbeChartPoint,
} from './tunnel-probe-series';

// The tunnel detail loads this module on demand, so the tunnel list never downloads ECharts.
echarts.use([
  LineChart,
  CustomChart,
  ScatterChart,
  GridComponent,
  TooltipComponent,
  MarkLineComponent,
  MarkAreaComponent,
  CanvasRenderer,
]);

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

/** First-byte time of each sampled Serving run across the selected window. */
export function TunnelProbeChart({
  points,
  windowSecs,
  endUnixSecs,
  bucketSecs,
  intervalSecs,
  p50Ms,
  p95Ms,
  slowMs,
}: {
  points: readonly TunnelProbeChartPoint[];
  windowSecs: number;
  endUnixSecs: number;
  bucketSecs: number;
  /** The scheduled cadence; null when only manual runs exist. */
  intervalSecs: number | null;
  p50Ms: number | null;
  p95Ms: number | null;
  slowMs: number;
}) {
  const [fullScale, setFullScale] = useState(false);
  const capAxis = observeValueAxis(Math.max(slowMs * 1.2, (p95Ms ?? 0) * 1.3));
  const overflow = points.filter(
    point => point.result === 'ok' && point.ttfb_ms !== null && point.ttfb_ms > capAxis.max,
  );
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
      p95Ms,
      fullScale,
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
    const buckets = tunnelProbeBuckets(points, bucketSecs, startMs, endMs);
    const missing = tunnelProbeMissingPeriods(buckets, intervalSecs);
    const valueAxis = fullScale
      ? observeValueAxis(Math.max(slowMs * 1.2, ...points.map(point => point.ttfb_ms ?? 0)))
      : observeValueAxis(Math.max(slowMs * 1.2, (p95Ms ?? 0) * 1.3));
    const axes = observeTimeAxes(startMs, endMs, ink3, lineSoft);
    const firstMs = points.length > 0 ? points[0].finished_at_unix_secs * 1000 : endMs;
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
        data: tunnelProbeLineData(buckets, missing, valueAxis.max),
        markLine: { silent: true, symbol: 'none', animation: false, data: markLines },
        markArea: {
          silent: true,
          animation: false,
          data: [
            ...missing.map(([start, end]) => [
              {
                xAxis: start,
                itemStyle: { color: ink, opacity: 0.045 },
                label: {
                  show: end - start > (windowSecs * 1000) / 35,
                  position: 'insideTop',
                  color: ink4,
                  fontSize: 9.5,
                  formatter: points.length ? '无已完成拨测' : '暂无 Serving 拨测',
                },
              },
              { xAxis: end },
            ]),
            ...tunnelProbeFailurePeriods(buckets).map(([start, end]) => [
              { xAxis: start, itemStyle: { color: err, opacity: 0.1 } },
              { xAxis: end },
            ]),
          ],
        },
      },
      {
        name: '超出纵轴',
        type: 'scatter',
        data: points
          .filter(point => point.result === 'ok' && point.ttfb_ms !== null && point.ttfb_ms > valueAxis.max)
          .map(point => [point.finished_at_unix_secs * 1000, valueAxis.max]),
        symbol: 'triangle',
        symbolSize: 7,
        symbolOffset: [0, 4],
        clip: false,
        z: 6,
        itemStyle: { color: gold },
      },
      {
        name: '时段状态',
        type: 'custom',
        encode: { x: [1, 2] },
        xAxisIndex: 2,
        yAxisIndex: 1,
        animation: false,
        data: buckets.map(bucket => [(bucket.start + bucket.end) / 2, bucket.start, bucket.end]),
        renderItem: (
          params: { dataIndex: number; coordSys: unknown },
          api: { value: (dimension: number) => number; coord: (value: number[]) => number[] },
        ) => {
          const area = params.coordSys as { x: number; y: number; width: number; height: number };
          const left = Math.max(area.x, api.coord([api.value(1), 0])[0]);
          const right = Math.min(area.x + area.width, api.coord([api.value(2), 0])[0]);
          const point = buckets[params.dataIndex].point;
          const fill = !point
            ? ink
            : point.result !== 'ok'
              ? err
              : point.ttfb_ms !== null && point.ttfb_ms > slowMs
                ? gold
                : cv('--ok');
          return {
            type: 'rect',
            shape: {
              x: left,
              y: area.y,
              width: Math.max(0, right - left - (right - left > 3 ? 1 : 0)),
              height: area.height,
            },
            style: {
              fill,
              opacity: !point ? 0.07 : point.result === 'ok' && (point.ttfb_ms ?? 0) <= slowMs ? 0.5 : 0.9,
            },
          };
        },
      },
    ];

    chart.setOption(
      {
        ...echartsEntranceAnimation(!hasRenderedData.current),
        axisPointer: { link: [{ xAxisIndex: [0, 2] }] },
        grid: [
          { left: 42, right: 14, top: 14, bottom: 54 },
          { left: 42, right: 14, bottom: 35, height: 10 },
        ],
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
            const bucket = buckets.find(bucket => atMs >= bucket.start && atMs < bucket.end);
            if (!bucket) return '';
            const point = bucket.point;
            const heading = `${clock(bucket.start)}–${clock(bucket.end)}`;
            const reading = !point
              ? '无已完成的 Serving 拨测'
              : point.result !== 'ok'
                ? `本时段有失败 · ${FAILURE_TEXT[point.result]}`
                : point.ttfb_ms === null
                  ? '成功 · 首字节未知'
                  : `抽样首字节 ${point.ttfb_ms} ms${point.ttfb_ms > slowMs ? ' · 变慢' : ''}`;
            const detail = point
              ? `<br><span>${clock(point.finished_at_unix_secs * 1000)} · ${point.result === 'ok' ? '该时段最近一次成功' : '该时段最近一次失败'}</span>`
              : '';
            return `<div style="color:${ink4};font-size:9px;margin-bottom:3px">${heading}</div><b style="color:${point && point.result !== 'ok' ? err : ink};font-weight:600">${reading}</b>${detail}`;
          },
        },
        xAxis: [
          { ...axes[0], gridIndex: 0, axisLabel: { show: false } },
          { ...axes[1], gridIndex: 0 },
          { ...axes[0], gridIndex: 1, splitLine: { show: false }, axisLine: { show: false } },
          { ...axes[1], gridIndex: 1 },
        ],
        yAxis: [
          {
            type: 'value',
            min: 0,
            max: valueAxis.max,
            interval: valueAxis.interval,
            axisLabel: { color: ink3, fontSize: 9.5, margin: 8, formatter: observeMsUnit(valueAxis.interval).text },
            axisLine: observeAxisLine(ink3),
            axisTick: observeAxisTick(ink3),
            splitLine: { show: true, lineStyle: { color: lineSoft, width: 1 } },
          },
          { type: 'value', gridIndex: 1, min: 0, max: 1, show: false },
        ],
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
    p95Ms,
    fullScale,
    slowMs,
    themeName,
    paletteKey,
    enteredViewport,
  ]);

  return (
    <>
      <div ref={elRef} className="tnd-chart" role="img" aria-label="首字节走势与拨测时段状态" />
      <div className="tnd-chart-legend" aria-label="图表说明">
        <span>
          <i className="ok" />
          成功样本
        </span>
        <span>
          <i className="warn" />
          变慢样本
        </span>
        <span>
          <i className="err" />
          时段有失败
        </span>
        <span>
          <i className="idle" />
          无已完成拨测
        </span>
        {overflow.length > 0 && (
          <button
            type="button"
            className="tnd-scale"
            aria-pressed={fullScale}
            onClick={() => setFullScale(value => !value)}
          >
            {fullScale ? '收起纵轴' : `▲ ${overflow.length} 个尖峰 · 完整纵轴`}
          </button>
        )}
        <span className="tnd-chart-sampling">每格 {bucketSecs / 60} 分钟；有失败时标红，其余显示最近一次成功样本</span>
      </div>
    </>
  );
}
