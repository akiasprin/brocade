import type { TunnelProbeView } from './api';

export type TunnelProbeChartPoint = TunnelProbeView['points'][number];
export interface TunnelProbeBucket {
  start: number;
  end: number;
  point: TunnelProbeChartPoint | null;
}

/** Console partitions by Unix time, independently of the requested window's moving start.
 * A failure is the latest failed run in a bucket; otherwise the point is its latest success. */
export function tunnelProbeBuckets(
  points: readonly TunnelProbeChartPoint[],
  bucketSecs: number,
  startMs: number,
  endMs: number,
): TunnelProbeBucket[] {
  const step = bucketSecs * 1000;
  const byBucket = new Map(points.map(point => [Math.floor(point.finished_at_unix_secs / bucketSecs), point]));
  const buckets: TunnelProbeBucket[] = [];
  for (let at = Math.floor(startMs / step) * step; at < endMs; at += step) {
    buckets.push({
      start: Math.max(startMs, at),
      end: Math.min(endMs, at + step),
      point: byBucket.get(at / step) ?? null,
    });
  }
  return buckets;
}

/** Empty buckets are observations of missing data, rather than a duration inferred from two
 * representative timestamps. A failure-first sample may be early in a completely occupied bucket.
 * Suppress shading for the ordinary wait between sparse scheduled probes; the status lane still
 * shows every empty bucket. Include the beginning and the live end of the window. */
export function tunnelProbeMissingPeriods(
  buckets: readonly TunnelProbeBucket[],
  intervalSecs: number | null,
): Array<[number, number]> {
  const periods: Array<[number, number]> = [];
  for (const bucket of buckets) {
    if (bucket.point) continue;
    const last = periods.at(-1);
    if (last?.[1] === bucket.start) last[1] = bucket.end;
    else periods.push([bucket.start, bucket.end]);
  }
  if (buckets.every(bucket => bucket.point === null)) return periods;
  return periods.filter(([start, end]) => end - start > (intervalSecs ?? 0) * 1000);
}

export function tunnelProbeFailurePeriods(buckets: readonly TunnelProbeBucket[]): Array<[number, number]> {
  const periods: Array<[number, number]> = [];
  for (const bucket of buckets) {
    if (!bucket.point || bucket.point.result === 'ok') continue;
    const last = periods.at(-1);
    if (last?.[1] === bucket.start) last[1] = bucket.end;
    else periods.push([bucket.start, bucket.end]);
  }
  return periods;
}

export function tunnelProbeLineData(
  buckets: readonly TunnelProbeBucket[],
  missing: readonly [number, number][],
  cap: number,
): Array<[number, number | null]> {
  const data: Array<[number, number | null]> = buckets.flatMap(bucket => {
    const point = bucket.point;
    return point
      ? [
          [
            point.finished_at_unix_secs * 1000,
            point.result === 'ok' && point.ttfb_ms !== null ? Math.min(point.ttfb_ms, cap) : null,
          ],
        ]
      : [];
  });
  for (const [start, end] of missing) data.push([(start + end) / 2, null]);
  return data.sort(([a], [b]) => a - b);
}
