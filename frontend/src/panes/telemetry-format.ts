import { observeBpsUnit, observeValueAxis } from '../ui/observe-chart';
import type { ObserveAxisUnit, ObserveValueAxis } from '../ui/observe-chart';

/** Unix seconds to the ISO shape consumed by Ago. */
export const iso = (secs: number) => new Date(secs * 1000).toISOString();

/** Compact duration used by the machine host and process summaries. */
export function dur(secs: number): string {
  if (secs < 60) return `${Math.round(secs)} 秒`;
  if (secs < 3600) return `${Math.floor(secs / 60)} 分钟`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} 小时`;
  return `${Math.floor(secs / 86400)} 天`;
}

/** One peak calculation shared by a throughput chart, its heading and its legend. */
export function throughputAxis(
  rx: (number | null)[],
  tx: (number | null)[],
): { axis: ObserveValueAxis; unit: ObserveAxisUnit } {
  const peak = Math.max(
    ...rx.filter((value): value is number => value !== null),
    ...tx.filter((value): value is number => value !== null),
    1,
  );
  const axis = observeValueAxis(peak);
  return { axis, unit: observeBpsUnit(axis) };
}
