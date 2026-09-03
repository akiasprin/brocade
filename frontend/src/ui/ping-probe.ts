import type { PingProbePoint } from '../api';

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
