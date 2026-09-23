import type { NodeRealtimeState } from './node-realtime';

export interface VpngateRealtimePool {
  outbound_id: string;
  country_code: string;
  state: 'pending' | 'healthy' | 'degraded' | 'failing_over' | 'failed';
  reason?: string;
  active_slot?: number;
  ready_standbys: number;
  candidate_count: number;
  consecutive_failures: number;
  last_success_age_millis?: number;
  probes: number;
  probe_failures: number;
  failovers: number;
  refill_attempts: number;
  refill_failures: number;
  refill_backoff_remaining_millis: number;
}

export interface VpngateRealtimeBackend {
  outbound_id: string;
  slot: number;
  role: 'active' | 'standby';
  state: 'starting' | 'healthy' | 'unhealthy' | 'backoff';
  reason?: string;
  server_id: string;
  last_success_age_millis?: number;
  consecutive_failures: number;
  backoff_remaining_millis: number;
}

export interface VpngateRealtimeReport {
  boot_id: string;
  sequence: number;
  sampled_at_unix_millis: number;
  pools: VpngateRealtimePool[];
  backends: VpngateRealtimeBackend[];
  events: {
    sequence: number;
    at_unix_millis: number;
    outbound_id: string;
    kind: string;
    from_slot?: number;
    to_slot?: number;
    reason?: string;
    recovery_elapsed_millis?: number;
  }[];
}

export function freshVpngateReport({ last, now, connected }: NodeRealtimeState): VpngateRealtimeReport | null {
  const report = last?.event.sample.vpngate;
  if (
    !report ||
    !connected ||
    !last ||
    now - last.arrived > 15_000 ||
    now - last.event.received_at_unix_millis > 15_000 ||
    last.event.sample.sampled_at_unix_millis - report.sampled_at_unix_millis > 15_000
  ) {
    return null;
  }
  return report;
}
