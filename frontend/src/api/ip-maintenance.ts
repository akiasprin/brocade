import { api } from './client';

export interface MaintenanceRange {
  min: number;
  max: number;
}
export interface MaintenanceBehavior {
  google_actions: MaintenanceRange;
  google_dwell_secs: MaintenanceRange;
  trust_actions: MaintenanceRange;
  trust_dwell: { weight: number; seconds: MaintenanceRange }[];
  android_actions: number[];
  apple_actions: number[];
  desktop_actions: number[];
  referer_percent: number;
  request_timeout_secs: number;
  timer_jitter_secs: number;
  runner_jitter_secs: number;
  cookie_idle_days: number;
}
export interface IpMaintenancePolicy {
  enabled: boolean;
  region: string;
  family: 'v4' | 'v6' | 'dual';
  source_ip: string | null;
  expected_public_ip: string | null;
  source_ipv6?: string | null;
  expected_public_ipv6?: string | null;
  google: boolean;
  trust: boolean;
  google_percent: number;
  interval_minutes: number;
  daily_requests: number;
  daily_download_mib: number;
  cooldown_minutes: number;
  continue_offline: boolean;
  behavior: MaintenanceBehavior;
}
export interface IpMaintenanceDesired {
  generation: number;
  policy: IpMaintenancePolicy;
  control_sequence: number;
  manual_request_id: string | null;
  stop_sequence: number;
  target_round_id: string | null;
  manual_sequence: number;
  manual_expires_at: number | null;
  session_generation: number;
  report_sequence_floor: number;
  data_digest?: string | null;
}
export type MaintenancePhase =
  | 'disabled'
  | 'waiting'
  | 'jitter'
  | 'preflight'
  | 'requesting'
  | 'dwelling'
  | 'postcheck'
  | 'complete'
  | 'interrupted'
  | 'cooldown'
  | 'budget_wait'
  | 'blocked_egress'
  | 'failed';
export interface MaintenanceAction {
  index: number;
  kind: string;
  host: string;
  status: number | null;
  error: string | null;
  response_bytes: number;
}
export interface IpMaintenanceReport {
  family?: 'v4' | 'v6' | null;
  generation: number;
  run_generation: number;
  agent_instance_id: string;
  applied_control_sequence: number;
  applied_stop_sequence: number;
  applied_session_generation: number;
  worker_observed_at_unix_secs: number;
  last_started_at_unix_secs: number | null;
  cooldown_until_unix_secs: number | null;
  sequence: number;
  phase: MaintenancePhase;
  round_id: string | null;
  module: 'google' | 'trust' | null;
  actual_exit_ip: string | null;
  data_digest?: string | null;
  actions_total: number;
  action_index: number;
  phase_deadline_unix_secs: number | null;
  next_run_unix_secs: number | null;
  daily_requests: number;
  daily_response_bytes: number;
  actions: MaintenanceAction[];
  evidence: { source: string; country: string | null; status: number | null }[];
  evidence_observed_at_unix_secs?: number | null;
  reason: string | null;
  observed_at_unix_secs: number;
}
export interface IpMaintenanceView {
  default_policy?: IpMaintenancePolicy;
  desired: IpMaintenanceDesired;
  supported: boolean;
  session_reset_supported?: boolean;
  dual_stack_supported?: boolean;
  family_reports?: IpMaintenanceReport[];
  lifecycle_active: boolean;
  report: IpMaintenanceReport | null;
  received_at_unix_secs: number | null;
  data_updated_at_unix_secs?: number | null;
  agents_updated_at_unix_secs?: number | null;
  data_error?: string | null;
}
export interface MaintenanceRegion {
  id: string;
  country: string;
  state: string;
  city: string;
  name: string;
  language_params: string;
  latitude: number;
  longitude: number;
}
const endpoint = (node: string) => `/nodes/${encodeURIComponent(node)}/ip-maintenance`;
export async function fetchIpMaintenance(node: string, signal?: AbortSignal): Promise<IpMaintenanceView> {
  const view = await api<IpMaintenanceView>(endpoint(node), '', { signal });
  if (
    !view?.desired?.policy?.behavior ||
    !Array.isArray(view.desired.policy.behavior.trust_dwell) ||
    typeof view.supported !== 'boolean' ||
    (view.family_reports !== undefined &&
      (!Array.isArray(view.family_reports) ||
        view.family_reports.length > 2 ||
        view.family_reports.some(r => !r || !Array.isArray(r.actions) || !Array.isArray(r.evidence)))) ||
    typeof view.lifecycle_active !== 'boolean' ||
    (view.report && (!Array.isArray(view.report.actions) || !Array.isArray(view.report.evidence)))
  ) {
    throw new Error('养护接口返回无效数据');
  }
  return view;
}
export async function fetchMaintenanceRegions(): Promise<MaintenanceRegion[]> {
  const regions = await api<MaintenanceRegion[]>('/ip-maintenance/regions');
  if (
    !Array.isArray(regions) ||
    regions.length > 100 ||
    regions.some(r => typeof r?.id !== 'string' || typeof r.name !== 'string' || !/^[A-Z]{2}$/.test(r.country))
  )
    throw new Error('养护地区目录返回无效数据');
  return regions;
}
export const saveIpMaintenance = (node: string, generation: number, policy: IpMaintenancePolicy) =>
  api<void>(endpoint(node), '', { method: 'PUT', body: JSON.stringify({ expected_generation: generation, policy }) });
export const commandIpMaintenance = (
  node: string,
  generation: number,
  command: 'run' | 'session-reset',
  requestId: string,
) =>
  api<void>(`${endpoint(node)}/${command}`, '', {
    method: 'POST',
    body: JSON.stringify({ expected_generation: generation, request_id: requestId }),
  });

export interface IpMaintenanceRuns {
  runs: IpMaintenanceReport[];
  next_cursor: number | null;
}
export const stopIpMaintenance = (
  node: string,
  generation: number,
  roundId: string,
  requestId: string,
  signal?: AbortSignal,
) =>
  api<void>(`${endpoint(node)}/stop`, '', {
    signal,
    method: 'POST',
    body: JSON.stringify({ expected_generation: generation, round_id: roundId, request_id: requestId }),
  });
export const fetchIpMaintenanceRuns = (node: string, cursor: number | null, signal?: AbortSignal) =>
  api<IpMaintenanceRuns>(`${endpoint(node)}/runs${cursor == null ? '' : `?cursor=${cursor}`}`, '', { signal });
export const fetchIpMaintenanceRun = (node: string, run: string, signal?: AbortSignal) =>
  api<IpMaintenanceReport>(`${endpoint(node)}/runs/${encodeURIComponent(run)}`, '', { signal });
