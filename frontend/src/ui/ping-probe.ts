import type { PingProbeFamily, PingProbePoint, PingProbeSkipReason, PingProbeTarget } from '../api';

export const PING_FAMILY_LABEL: Record<PingProbeFamily, string> = { ipv4: 'IPv4', ipv6: 'IPv6' };
export const MAX_PING_PROBE_TARGETS = 6;

/** 北京、上海、重庆按 2026-10-19 ITDOG 当地测点延迟核对；广东于 2026-10-28 更新，电信另经远端双栈复测。地域不取 IPv6 地址库的默认标注。 */
export const PING_PROBE_PRESETS = [
  {
    label: '北京',
    source: '电信、移动：百度；联通：微信静态资源 CDN',
    carriers: [
      { name: '北京电信', ipv4: '220.181.111.232', ipv6: '240e:83:205:1cd:0:ff:b0b8:dee9' },
      { name: '北京联通', ipv4: '123.125.246.106', ipv6: '2408:80f0:4105:6:40::100' },
      { name: '北京移动', ipv4: '39.156.70.46', ipv6: '2409:8c00:6c21:118b:0:ff:b0e8:f003' },
    ],
  },
  {
    label: '上海',
    source: 'QQ 首页 CDN',
    carriers: [
      { name: '上海电信', ipv4: '101.91.42.232', ipv6: '240e:e1:a800:120::76' },
      { name: '上海联通', ipv4: '58.246.163.58', ipv6: '2408:80f1:21:c120::c' },
      { name: '上海移动', ipv4: '183.194.238.117', ipv6: '2409:8c1e:75b0:1120::27' },
    ],
  },
  {
    label: '广东',
    source: '电信：微信头像 CDN；联通：阿里图片 / 微信头像 CDN；移动：腾讯视频 / QQ 首页 CDN',
    carriers: [
      { name: '广东电信', ipv4: '183.61.254.74', ipv6: '240e:97d:10:1705:3a::e' },
      { name: '广东联通', ipv4: '58.255.175.14', ipv6: '2408:8756:2cff:e409:3c::26' },
      { name: '广东移动', ipv4: '120.233.185.200', ipv6: '2409:8c54:871:4::87' },
    ],
  },
  {
    label: '重庆',
    source: '腾讯视频 CDN；移动 IPv4：微信头像 CDN',
    carriers: [
      { name: '重庆电信', ipv4: '183.66.105.224', ipv6: '240e:930:1000:1703:3e::1' },
      { name: '重庆联通', ipv4: '58.144.227.55', ipv6: '2408:8763:0:2b0:5e::1f' },
      { name: '重庆移动', ipv4: '111.10.45.113', ipv6: '2409:8c60:2600:ea:40::2' },
    ],
  },
] as const;

export function pingProbePresetTargets(preset: (typeof PING_PROBE_PRESETS)[number]): PingProbeTarget[] {
  return (['tcp', 'icmp'] as const).flatMap(kind =>
    preset.carriers.map(carrier => ({
      name: carrier.name,
      kind,
      ipv4: kind === 'tcp' ? `${carrier.ipv4}:80` : carrier.ipv4,
      ipv6: kind === 'tcp' ? `[${carrier.ipv6}]:80` : carrier.ipv6,
    })),
  );
}

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
