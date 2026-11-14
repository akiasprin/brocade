import type {
  IpMaintenanceDesired,
  IpMaintenancePolicy,
  IpMaintenanceReport,
  IpMaintenanceView,
  MaintenanceRegion,
} from '../../src/api';

export const maintenanceRegions: MaintenanceRegion[] = [
  {
    id: 'JP/Default/Tokyo',
    country: 'JP',
    state: 'Default',
    city: 'Tokyo',
    name: '日本（东京）',
    language_params: 'hl=ja&gl=JP',
    latitude: 35.6812,
    longitude: 139.7671,
  },
  {
    id: 'US/CA/Los_Angeles',
    country: 'US',
    state: 'CA',
    city: 'Los_Angeles',
    name: '美国（洛杉矶）',
    language_params: 'hl=en&gl=US',
    latitude: 34.0522,
    longitude: -118.2437,
  },
];
for (const [country, city, name] of [
  ['HK', 'HongKong', '香港'],
  ['SG', 'Singapore', '新加坡'],
])
  maintenanceRegions.push({ ...maintenanceRegions[0], id: `${country}/Default/${city}`, country, city, name });

export function maintenancePolicy(): IpMaintenancePolicy {
  return {
    enabled: true,
    region: 'JP/Default/Tokyo',
    family: 'v4',
    source_ip: null,
    expected_public_ip: null,
    google: true,
    trust: true,
    google_percent: 70,
    interval_minutes: 20,
    daily_requests: 1500,
    daily_download_mib: 128,
    cooldown_minutes: 60,
    continue_offline: false,
    behavior: {
      google_actions: { min: 5, max: 8 },
      google_dwell_secs: { min: 45, max: 75 },
      trust_actions: { min: 3, max: 6 },
      trust_dwell: [
        { weight: 45, seconds: { min: 8, max: 20 } },
        { weight: 35, seconds: { min: 20, max: 60 } },
        { weight: 15, seconds: { min: 60, max: 180 } },
        { weight: 5, seconds: { min: 180, max: 479 } },
      ],
      android_actions: [25, 30, 30, 15],
      apple_actions: [30, 35, 25, 10],
      desktop_actions: [20, 40, 20, 20],
      referer_percent: 70,
      request_timeout_secs: 15,
      timer_jitter_secs: 180,
      runner_jitter_secs: 179,
      cookie_idle_days: 14,
    },
  };
}
export function maintenanceReport(): IpMaintenanceReport {
  const now = Math.floor(Date.now() / 1000);
  return {
    generation: 12,
    sequence: 42,
    run_generation: 12,
    agent_instance_id: 'fixture-agent',
    applied_control_sequence: 0,
    applied_stop_sequence: 0,
    applied_session_generation: 0,
    worker_observed_at_unix_secs: now,
    last_started_at_unix_secs: now - 60,
    cooldown_until_unix_secs: null,
    phase: 'dwelling',
    round_id: 'fixture-round-19',
    module: 'google',
    actual_exit_ip: '192.0.2.25',
    actions_total: 5,
    action_index: 1,
    phase_deadline_unix_secs: now + 45,
    next_run_unix_secs: now + 1200,
    daily_requests: 28,
    daily_response_bytes: 1677722,
    actions: [
      { index: 1, kind: 'google_maps', host: 'www.google.com', status: 200, error: null, response_bytes: 40960 },
    ],
    evidence: [
      { source: 'google_jump', country: null, status: 200 },
      { source: 'youtube_premium', country: 'JP', status: 200 },
      { source: 'youtube_music', country: 'JP', status: 200 },
    ],
    reason: null,
    observed_at_unix_secs: now,
  };
}
export function maintenanceView(): IpMaintenanceView {
  return {
    default_policy: { ...maintenancePolicy(), enabled: false, interval_minutes: 45 },
    desired: {
      generation: 12,
      policy: maintenancePolicy(),
      control_sequence: 0,
      manual_request_id: null,
      stop_sequence: 0,
      target_round_id: null,
      manual_sequence: 0,
      manual_expires_at: null,
      session_generation: 0,
      report_sequence_floor: 42,
    },
    supported: true,
    session_reset_supported: true,
    dual_stack_supported: true,
    lifecycle_active: true,
    report: maintenanceReport(),
    received_at_unix_secs: Math.floor(Date.now() / 1000),
  };
}

export function maintenanceResetReport(desired: IpMaintenanceDesired): IpMaintenanceReport {
  return {
    ...maintenanceReport(),
    generation: desired.generation,
    run_generation: desired.generation,
    applied_control_sequence: desired.control_sequence,
    applied_stop_sequence: desired.stop_sequence,
    applied_session_generation: desired.session_generation,
    sequence: (desired.report_sequence_floor ?? 0) + 1,
    phase: 'disabled',
    round_id: null,
    module: null,
    actual_exit_ip: null,
    actions_total: 0,
    action_index: 0,
    actions: [],
    evidence: [],
    evidence_observed_at_unix_secs: null,
    daily_requests: 0,
    daily_response_bytes: 0,
    last_started_at_unix_secs: null,
    cooldown_until_unix_secs: null,
    phase_deadline_unix_secs: null,
    next_run_unix_secs: null,
    reason: null,
  };
}

export function maintenancePartialReport(): IpMaintenanceReport {
  return {
    ...maintenanceReport(),
    phase: 'complete',
    action_index: 4,
    actions_total: 4,
    phase_deadline_unix_secs: null,
    actions: [
      {
        index: 1,
        kind: 'google_news',
        host: 'news.google.com',
        status: 200,
        error: null,
        response_bytes: 6 * 1024 * 1024,
      },
      {
        index: 2,
        kind: 'google_news',
        host: 'news.google.com',
        status: null,
        error: 'response_size_skipped',
        response_bytes: 16 * 1024 * 1024 + 1,
      },
      {
        index: 3,
        kind: 'google_search',
        host: 'www.google.com',
        status: null,
        error: 'network_timeout',
        response_bytes: 0,
      },
      { index: 4, kind: 'google_maps', host: 'www.google.com', status: 200, error: null, response_bytes: 40960 },
    ],
  };
}
