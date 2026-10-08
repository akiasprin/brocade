import { describe, expect, it } from 'vitest';
import { observeSampleStepSecs, observeSeriesData } from '../src/ui/observe-series';

describe('observation time-series gaps', () => {
  it('accepts minute history followed by raw windows while preserving a real outage', () => {
    expect(
      observeSeriesData([60, 120, 150, 180, 300], [1, 2, 3, 4, 5], {
        windowStartsUnixSecs: [0, 60, 120, 150, 240],
      }),
    ).toEqual([
      [60_000, 1],
      [120_000, 2],
      [150_000, 3],
      [180_000, 4],
      [210_000, null],
      [300_000, 5],
    ]);
  });

  it('inserts a break when a complete run of point samples is absent', () => {
    const times = [100, 110, 1_000, 1_010];
    const values = [12, 13, 14, 15];
    expect(observeSeriesData(times, values)).toEqual([
      [100_000, 12],
      [110_000, 13],
      [555_000, null],
      [1_000_000, 14],
      [1_010_000, 15],
    ]);
    expect(times).toEqual([100, 110, 1_000, 1_010]);
    expect(values).toEqual([12, 13, 14, 15]);
  });

  it('allows ordinary scheduling jitter without changing real zeroes or nulls', () => {
    expect(observeSeriesData([100, 110, 121, 130, 140], [0, null, 1, Number.NaN, Number.POSITIVE_INFINITY])).toEqual([
      [100_000, 0],
      [110_000, null],
      [121_000, 1],
      [130_000, null],
      [140_000, null],
    ]);
  });

  it('uses actual window boundaries even with only two samples', () => {
    expect(observeSeriesData([130, 1_030], [10, 20], { windowStartsUnixSecs: [100, 1_000] })).toEqual([
      [130_000, 10],
      [565_000, null],
      [1_030_000, 20],
    ]);
  });

  it('does not mistake a longer but contiguous measured window for missing data', () => {
    expect(observeSeriesData([130, 160, 1_030], [10, 20, 30], { windowStartsUnixSecs: [100, 130, 160] })).toEqual([
      [130_000, 10],
      [160_000, 20],
      [1_030_000, 30],
    ]);
    expect(observeSeriesData([130, 160], [1, 2], { windowStartsUnixSecs: [100, 131] })).toHaveLength(2);
  });

  it('places the separator inside missing time, not inside the recovered measured window', () => {
    expect(observeSeriesData([130, 1_000], [10, 20], { windowStartsUnixSecs: [100, 140] })).toEqual([
      [130_000, 10],
      [135_000, null],
      [1_000_000, 20],
    ]);
  });

  it('keeps gap work bounded regardless of outage duration', () => {
    expect(observeSeriesData([0, 10, 31_536_000], [1, 2, 3])).toHaveLength(4);
  });

  it('does not add redundant separators next to explicit point-series nulls', () => {
    expect(observeSeriesData([100, 110, 115, 120], [12, null, null, 24])).toEqual([
      [100_000, 12],
      [110_000, null],
      [115_000, null],
      [120_000, 24],
    ]);
  });

  it('does not invent missing intervals without cadence evidence', () => {
    expect(observeSampleStepSecs([])).toBeNull();
    expect(observeSampleStepSecs([10, 10, 10])).toBeNull();
    expect(observeSampleStepSecs([10, 20, 1_000])).toBe(10);
    expect(observeSeriesData([], [])).toEqual([]);
    expect(observeSeriesData([10], [0])).toEqual([[10_000, 0]]);
    expect(observeSeriesData([10, 1_000], [1, 2])).toHaveLength(2);
  });
});
