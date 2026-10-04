import { describe, expect, it } from 'vitest';
import { latencyMean } from '../src/ui/latency';

describe('Chain latency average', () => {
  it('ignores an extreme high sample', () => {
    expect(latencyMean([95, 100, 102, 3_000, 105, 110])).toEqual({
      value: 102,
      includedSamples: 5,
      excludedSpikes: 1,
    });
  });

  it('ignores consecutive high outliers', () => {
    expect(latencyMean([95, 100, 102, 104, 3_000, 3_100, 105, 107, 110])).toEqual({
      value: 103,
      includedSamples: 7,
      excludedSpikes: 2,
    });
  });

  it('keeps ordinary jitter even when the other readings are tightly grouped', () => {
    expect(latencyMean([100, 101, 102, 150, 103, 104])).toEqual({
      value: 110,
      includedSamples: 6,
      excludedSpikes: 0,
    });
  });

  it('ignores a latest outlier without waiting for another probe', () => {
    expect(latencyMean([95, 100, 102, 105, 3_000])).toEqual({
      value: 101,
      includedSamples: 4,
      excludedSpikes: 1,
    });
  });

  it('ignores an outlier next to a failed probe but does not filter with too few successful samples', () => {
    expect(latencyMean([95, 100, 3_000, null, 105, 110])).toEqual({
      value: 103,
      includedSamples: 4,
      excludedSpikes: 1,
    });
    expect(latencyMean([100, 100, 100, 1_000])).toEqual({
      value: 325,
      includedSamples: 4,
      excludedSpikes: 0,
    });
  });

  it('leaves an empty successful window unknown', () => {
    expect(latencyMean([null, null])).toEqual({ value: null, includedSamples: 0, excludedSpikes: 0 });
  });
});
