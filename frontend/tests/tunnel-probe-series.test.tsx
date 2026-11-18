import { describe, expect, it } from 'vitest';
import {
  tunnelProbeBuckets,
  tunnelProbeFailurePeriods,
  tunnelProbeLineData,
  tunnelProbeMissingPeriods,
  type TunnelProbeChartPoint,
} from '../src/tunnel-probe-series';

const point = (at: number, result: TunnelProbeChartPoint['result'] = 'ok', ttfb = 160): TunnelProbeChartPoint => ({
  run_id: at,
  finished_at_unix_secs: at,
  result,
  ttfb_ms: result === 'ok' ? ttfb : null,
});

describe('Serving probe time buckets', () => {
  it('does not invent a gap when adjacent occupied buckets select an early failure and a late success', () => {
    const buckets = tunnelProbeBuckets([point(90, 'timeout'), point(4890)], 2520, 0, 5040000);
    expect(tunnelProbeMissingPeriods(buckets, 300)).toEqual([]);
    expect(tunnelProbeFailurePeriods(buckets)).toEqual([[0, 2520000]]);
    expect(tunnelProbeLineData(buckets, [], 400)).toEqual([
      [90000, null],
      [4890000, 160],
    ]);
  });

  it('clips epoch-aligned buckets to a moving window, including missing data at both ends', () => {
    const buckets = tunnelProbeBuckets([point(750)], 360, 100000, 1900000);
    const gaps = tunnelProbeMissingPeriods(buckets, 300);
    expect(buckets[0]).toMatchObject({ start: 100000, end: 360000 });
    expect(gaps).toEqual([
      [100000, 720000],
      [1080000, 1900000],
    ]);
    expect(tunnelProbeLineData(buckets, gaps, 400)).toContainEqual([1490000, null]);
  });

  it('shows an entirely unobserved window without manufacturing zeros', () => {
    const buckets = tunnelProbeBuckets([], 360, 0, 86400000);
    const gaps = tunnelProbeMissingPeriods(buckets, 3600);
    expect(gaps).toEqual([[0, 86400000]]);
    expect(tunnelProbeLineData(buckets, gaps, 400)).toEqual([[43200000, null]]);
  });

  it('allows ordinary spacing for hourly probes while keeping the lane buckets empty', () => {
    const buckets = tunnelProbeBuckets([point(10), point(3610)], 360, 0, 3960000);
    expect(buckets.filter(bucket => bucket.point === null)).toHaveLength(9);
    expect(tunnelProbeMissingPeriods(buckets, 3600)).toEqual([]);
  });

  it('clips latency for drawing while preserving the original reading for inspection', () => {
    const buckets = tunnelProbeBuckets([point(90, 'ok', 620)], 360, 0, 360000);
    expect(tunnelProbeLineData(buckets, [], 400)).toEqual([[90000, 400]]);
    expect(buckets[0].point?.ttfb_ms).toBe(620);
  });
});
