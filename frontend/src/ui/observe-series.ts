/** Typical spacing of retained points, in seconds. Ignore duplicates and use the lower median
 * so one missing run cannot become the cadence of an otherwise regular, short series. Without
 * window metadata this is an estimate: two isolated points cannot establish a missing interval. */
export function observeSampleStepSecs(times: readonly number[]): number | null {
  const steps: number[] = [];
  for (let index = 1; index < times.length; index += 1) {
    const step = times[index] - times[index - 1];
    if (Number.isFinite(step) && step > 0) steps.push(step);
  }
  steps.sort((left, right) => left - right);
  return steps.length === 0 ? null : steps[Math.floor((steps.length - 1) / 2)];
}

/** Preserve every real value and timestamp, adding one null separator per missing run. ECharts
 * connectNulls:false only breaks at explicit nulls, not at absent timestamps. One separator is
 * enough for both the line and area; never expand a long outage into thousands of empty points.
 * Window boundaries are authoritative when supplied. Point-only series allow half a cadence of
 * scheduling jitter before treating a larger interval as missing observations. */
export function observeSeriesData(
  timesUnixSecs: readonly number[],
  values: readonly (number | null)[],
  options: { windowStartsUnixSecs?: readonly number[]; sampleStepSecs?: number | null } = {},
): Array<[number, number | null]> {
  const step = options.windowStartsUnixSecs ? null : (options.sampleStepSecs ?? observeSampleStepSecs(timesUnixSecs));
  const data: Array<[number, number | null]> = [];
  timesUnixSecs.forEach((time, index) => {
    const previous = timesUnixSecs[index - 1];
    const windowStart = options.windowStartsUnixSecs?.[index];
    const disconnected =
      windowStart !== undefined
        ? windowStart > previous + 1 // Boundaries have whole-second precision.
        : step !== null && time - previous > step * 1.5;
    const value = values[index];
    const previousValue = values[index - 1];
    // Point series already break at nulls. Windowed (including stacked) series keep identical
    // separator positions even when just one metric has an additional missing value.
    const needsSeparator =
      windowStart !== undefined ||
      (value != null && Number.isFinite(value) && previousValue != null && Number.isFinite(previousValue));
    if (index > 0 && disconnected && needsSeparator) {
      data.push([((previous + (windowStart ?? time)) / 2) * 1000, null]);
    }
    data.push([time * 1000, value != null && Number.isFinite(value) ? value : null]);
  });
  return data;
}
