import type { PingProbeFamily, PingProbePoint, PingProbeSkipReason } from '../api';

export const PING_FAMILY_LABEL: Record<PingProbeFamily, string> = { ipv4: 'IPv4', ipv6: 'IPv6' };

export function pingLatencyMs(sample: Pick<PingProbePoint, 'latency_us'>): number | null {
  return sample.latency_us == null ? null : sample.latency_us / 1000;
}

export function pingLatencyText(value: number): string {
  if (value < 1) return `${value.toFixed(2)} ms`;
  if (value < 10) return `${value.toFixed(1)} ms`;
  return `${Math.round(value)} ms`;
}

export function pingSampleText(sample: PingProbePoint | undefined): string {
  if (!sample) return '—';
  if (!sample.attempted) return '未探测';
  const latency = pingLatencyMs(sample);
  return latency == null ? '无响应' : pingLatencyText(latency);
}

/** An attempted probe that got no reply within the timeout: packet loss, as opposed to a gap. */
export const pingSampleLost = (sample: PingProbePoint | undefined) =>
  sample !== undefined && sample.attempted && sample.latency_us == null;

/** Summarize the queried range, not its latest point. Capability gaps and missing reports are
 * unknown observations, not failed probes, so neither belongs in the loss denominator. */
export function pingLossStats(samples: readonly PingProbePoint[]) {
  let attempted = 0;
  let lost = 0;
  for (const sample of samples) {
    if (!sample.attempted) continue;
    attempted += 1;
    if (sample.latency_us == null) lost += 1;
  }
  return { attempted, lost, percent: attempted === 0 ? null : (lost / attempted) * 100 };
}

/** 丢包程度：部分丢包与已探测的全部无响应分开着色；没有丢包或没有探测时不着色。 */
export type PingLossTone = 'partial' | 'down';

export function pingLossTone(stats: { attempted: number; lost: number }): PingLossTone | null {
  if (stats.lost === 0) return null;
  return stats.lost === stats.attempted ? 'down' : 'partial';
}

/** The more severe of several tones, for a summary over several series. */
export function worstPingLossTone(tones: Iterable<PingLossTone | null>): PingLossTone | null {
  let worst: PingLossTone | null = null;
  for (const tone of tones) {
    if (tone === 'down') return 'down';
    if (tone === 'partial') worst = 'partial';
  }
  return worst;
}

export function pingLossText(percent: number | null): string {
  if (percent === null) return '—';
  // A long range can have enough samples for rounding to hide a real loss (or response).
  if (percent > 0 && percent < 0.01) return '<0.01%';
  if (percent < 100 && percent > 99.99) return '>99.99%';
  return `${Number(percent.toFixed(2))}%`;
}

/** The one reason every sample of a range went unprobed; null when something was probed, when the
 * reasons differ, or when the Agent predates skip reasons. */
export function pingSkipReason(samples: readonly PingProbePoint[]): PingProbeSkipReason | null {
  let reason: PingProbeSkipReason | null = null;
  for (const sample of samples) {
    if (sample.attempted || !sample.skip_reason) return null;
    if (reason !== null && reason !== sample.skip_reason) return null;
    reason = sample.skip_reason;
  }
  return reason;
}

/** 图例里的短写。 */
export function pingSkipReasonShort(reason: PingProbeSkipReason, family: PingProbeFamily): string {
  switch (reason) {
    case 'no_route':
      return '无路由';
    case 'no_address':
      return family === 'ipv4' ? '无 A' : '无 AAAA';
    case 'resolve_failed':
      return '解析失败';
    case 'unavailable':
      return '未探测';
  }
}

/** 悬停标题里的完整说明。 */
export function pingSkipReasonText(reason: PingProbeSkipReason, family: PingProbeFamily): string {
  switch (reason) {
    case 'no_route':
      return `机器没有 ${PING_FAMILY_LABEL[family]} 路由`;
    case 'no_address':
      return family === 'ipv4' ? '域名没有 A 记录' : '域名没有 AAAA 记录';
    case 'resolve_failed':
      return '域名解析失败';
    case 'unavailable':
      return '机器无法发起该探测';
  }
}
