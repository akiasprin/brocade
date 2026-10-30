import { act, cleanup, render } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import SiteUsageChart from '../src/site-usage-chart';

const mock = vi.hoisted(() => ({
  pixelsPerBucket: 10,
  resized: () => {},
  chart: {
    setOption: vi.fn(),
    convertToPixel: vi.fn(),
    resize: vi.fn(),
    dispose: vi.fn(),
    dispatchAction: vi.fn(),
  },
}));
vi.mock('echarts/core', () => ({ use: vi.fn(), init: () => mock.chart }));
vi.mock('../src/ui/echarts-motion', () => ({
  useEchartsViewportEntry: () => true,
  echartsEntranceAnimation: () => ({ animation: false }),
}));

const props = {
  period: 'day' as const,
  startUnixSecs: 1_792_166_400,
  bucketSecs: 3600,
  nowUnixSecs: 1_792_166_400 + 6 * 3600,
  observedSinceUnixSecs: 1_792_166_400 + 2 * 3600,
  series: [{ key: 'youtube', name: 'YouTube', color: { token: '--observe-1' }, values: [0, 0, 1024, 0] }],
  focusKey: null,
};
beforeEach(() => {
  mock.pixelsPerBucket = 10;
  mock.chart.setOption.mockClear();
  mock.chart.convertToPixel.mockImplementation((_axis: unknown, value: number) => value * mock.pixelsPerBucket);
  vi.stubGlobal(
    'ResizeObserver',
    class {
      constructor(callback: () => void) {
        mock.resized = callback;
      }
      observe() {}
      disconnect() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it('fits the observation label to actual plot width on initial layout and both resize directions', () => {
  render(<SiteUsageChart {...props} />);
  expect(mock.chart.setOption.mock.calls[0][0].series[0].markArea.label.show).toBe(false);
  expect(mock.chart.setOption).toHaveBeenCalledTimes(1);
  mock.pixelsPerBucket = 30;
  act(() => mock.resized());
  expect(mock.chart.setOption).toHaveBeenLastCalledWith({
    series: [{ id: 'youtube', markArea: { label: { show: true } } }],
  });
  act(() => mock.resized());
  expect(mock.chart.setOption).toHaveBeenCalledTimes(2);
  mock.pixelsPerBucket = 10;
  act(() => mock.resized());
  expect(mock.chart.setOption).toHaveBeenLastCalledWith({
    series: [{ id: 'youtube', markArea: { label: { show: false } } }],
  });
});

it('distinguishes unobserved buckets, observed zeroes and future buckets in the tooltip', () => {
  render(<SiteUsageChart {...props} />);
  const formatter = mock.chart.setOption.mock.calls[0][0].tooltip.formatter;
  expect(formatter([{ seriesIndex: 0, value: [0.5, 0] }])).toContain('未观测');
  expect(formatter([{ seriesIndex: 0, value: [3.5, 0] }])).toContain('无流量');
  expect(formatter([{ seriesIndex: 0, value: [7.5, null] }])).toBe('');
});

it('keeps retained history visible and distinguishes unknown coverage from observed zeroes', () => {
  render(
    <SiteUsageChart {...props} observedSinceUnixSecs={null} uncertainBeforeUnixSecs={props.observedSinceUnixSecs} />,
  );
  const option = mock.chart.setOption.mock.calls[0][0];
  expect(option.series[0].markArea).toBeUndefined();
  expect(option.series[0].data[2]).toEqual([2.5, 1024]);
  expect(option.tooltip.formatter([{ seriesIndex: 0, value: [0.5, 0] }])).toContain('无记录 · 采集可能不完整');
  expect(option.tooltip.formatter([{ seriesIndex: 0, value: [0.5, 1024] }])).not.toContain('未观测');
  expect(option.tooltip.formatter([{ seriesIndex: 0, value: [3.5, 0] }])).toContain('无流量');
});

it('lists forwarding first in the tooltip and keeps the other categories in stack order', () => {
  render(
    <SiteUsageChart
      {...props}
      series={[
        { key: 'forwarding', name: '端口转发', color: { token: '--observe-2' }, values: [10, 0, 0, 0] },
        ...props.series,
        { key: 'github', name: 'GitHub', color: { token: '--observe-6' }, values: [100, 0, 0, 0] },
      ]}
    />,
  );
  const formatter = mock.chart.setOption.mock.calls[0][0].tooltip.formatter;
  const text: string = formatter([
    { seriesIndex: 0, value: [0.5, 10] },
    { seriesIndex: 1, value: [0.5, 1000] },
    { seriesIndex: 2, value: [0.5, 100] },
  ]);
  for (const name of ['端口转发', 'GitHub', 'YouTube']) expect(text).toContain(name);
  expect(text.indexOf('端口转发')).toBeLessThan(text.indexOf('GitHub'));
  expect(text.indexOf('GitHub')).toBeLessThan(text.indexOf('YouTube'));
});

const at = (year: number, month: number, day: number, hour = 0) => Date.UTC(year, month - 1, day, hour - 8) / 1000;

it.each([4, 36, 31 * 24])('uses distinct UTC+8 ticks for a custom range of %i hours', hours => {
  const start = at(2026, 10, 1, 6);
  render(
    <SiteUsageChart
      {...props}
      customRange
      startUnixSecs={start}
      nowUnixSecs={start + hours * 3600}
      observedSinceUnixSecs={null}
      series={[{ ...props.series[0], values: Array(hours).fill(1024) }]}
    />,
  );
  const axis = mock.chart.setOption.mock.calls[0][0].xAxis;
  const ticks: number[] = axis.axisLabel.customValues;
  const labels: string[] = ticks.map(axis.axisLabel.formatter);
  expect(ticks.length).toBeGreaterThanOrEqual(2);
  expect(ticks.length).toBeLessThanOrEqual(8);
  expect(ticks.every(tick => tick >= 0 && tick <= hours)).toBe(true);
  expect(new Set(labels).size).toBe(labels.length);
  if (hours > 24) expect(labels.every(label => /^\d{2}-\d{2}/.test(label))).toBe(true);
  else expect(labels[0]).toBe('06:00');
});

it('names both ends of a past custom bucket instead of calling the range end now', () => {
  const start = at(2026, 10, 17, 6);
  render(
    <SiteUsageChart
      {...props}
      customRange
      startUnixSecs={start}
      nowUnixSecs={at(2026, 10, 18, 18)}
      series={[{ ...props.series[0], values: Array(36).fill(1024) }]}
    />,
  );
  const formatter = mock.chart.setOption.mock.calls[0][0].tooltip.formatter;
  const text: string = formatter([{ seriesIndex: 0, value: [35.5, 1024] }]);
  expect(text).toContain('10-18 17:00 – 10-18 18:00');
  expect(text).not.toContain('现在');
});

it('includes years in custom ticks and tooltip intervals across New Year', () => {
  const start = at(2026, 12, 31, 6);
  render(
    <SiteUsageChart
      {...props}
      customRange
      startUnixSecs={start}
      nowUnixSecs={at(2027, 1, 1, 18)}
      series={[{ ...props.series[0], values: Array(36).fill(1024) }]}
    />,
  );
  const option = mock.chart.setOption.mock.calls[0][0];
  expect(option.xAxis.axisLabel.formatter(0)).toBe('2026-12-31\n06:00');
  expect(option.xAxis.axisLabel.formatter(36)).toBe('2027-01-01\n18:00');
  expect(option.tooltip.formatter([{ seriesIndex: 0, value: [17.5, 1024] }])).toContain(
    '2026-12-31 23:00 – 2027-01-01 00:00',
  );
});
