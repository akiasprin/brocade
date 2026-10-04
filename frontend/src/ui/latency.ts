export interface LatencyMean {
  value: number | null;
  includedSamples: number;
  excludedSpikes: number;
}

const MIN_OUTLIER_SAMPLES = 5;
const EXTREME_IQR_MULTIPLIER = 3;
const MIN_RELATIVE_SPIKE = 1.5;

function quantile(sorted: readonly number[], fraction: number): number {
  const position = (sorted.length - 1) * fraction;
  const lower = Math.floor(position);
  const upper = Math.ceil(position);
  if (lower === upper) return sorted[lower];
  const weight = position - lower;
  return sorted[lower] * (1 - weight) + sorted[upper] * weight;
}

/** Return a mean that excludes extreme high-latency outliers identified by an IQR fence. */
export function latencyMean(samples: readonly (number | null)[]): LatencyMean {
  const successful = samples.filter((value): value is number => value != null);
  if (successful.length === 0) return { value: null, includedSamples: 0, excludedSpikes: 0 };

  let included = successful;
  if (successful.length >= MIN_OUTLIER_SAMPLES) {
    const sorted = [...successful].sort((left, right) => left - right);
    const firstQuartile = quantile(sorted, 0.25);
    const thirdQuartile = quantile(sorted, 0.75);
    const upperFence = Math.max(
      thirdQuartile + EXTREME_IQR_MULTIPLIER * (thirdQuartile - firstQuartile),
      thirdQuartile * MIN_RELATIVE_SPIKE,
    );

    included = successful.filter(value => value <= upperFence);
  }

  return {
    value: Math.round(included.reduce((sum, value) => sum + value, 0) / included.length),
    includedSamples: included.length,
    excludedSpikes: successful.length - included.length,
  };
}
