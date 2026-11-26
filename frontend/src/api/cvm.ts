import { api } from './client';
import type { CvmAllocatedNic, CvmContainerRoutes } from './cvm-nics';
import type { UserTrafficPolicyView } from '../api';

// CVM: system image, resources and network attachments are independent contracts.
export interface CvmImage {
  id: string;
  name: string;
  version: string;
  architecture: 'x86_64' | 'aarch64';
  sha256: string;
  boot_mode: string;
}
export interface CvmNetworkSource {
  max_attachments?: number;
  supports_ipv6?: boolean;
  id: string;
  name: string;
  kind: 'public';
  description: string;
}
export interface CvmHostIdentity {
  machine: string;
  root_filesystem: string;
}
export interface CvmHostInventory {
  identity: CvmHostIdentity;
  os: string;
  terminal_user: string;
  primary_device: string;
}
export interface CvmAdopt {
  request_id: string;
  tenant_id: string;
  user_id: string | null;
  node_id: string;
  name: string;
  identity: CvmHostIdentity;
}
export const adoptCvmHost = (request: CvmAdopt) =>
  api<CvmInstance>('/cvm/adopt', '', { method: 'POST', body: JSON.stringify(request) });
export interface CvmCapabilities {
  baremetal?: CvmHostInventory;
  host_system?: Pick<CvmHostInventory, 'os' | 'primary_device'> & {
    identity?: CvmHostIdentity;
    terminal_user?: string;
    resources?: CvmResources;
    terminal_ready?: boolean;
  };
  host_system_error?: string;
  network_quota?: CvmNetworkQuotaCapacity | null;
  managed_networks: boolean;
  guest_terminal: boolean;
  guest_terminal_persistent: boolean;
  primary_ip_delivery: boolean;
  resource_limits: CvmResourceLimits | null;
  backend: string;
  create: boolean;
  start: boolean;
  stop: boolean;
  delete: boolean;
  stable_storage: boolean;
  retain_on_delete: boolean;
  restart: boolean;
  reinstall: boolean;
  configure: boolean;
  configure_network: boolean;
  chain_egress: boolean;
  route_services: boolean;
  hot_attach: boolean;
  hot_detach: boolean;
  max_interfaces: number;
  allocatable_vcpus: number;
  allocatable_memory_bytes: number;
  allocatable_disk_bytes: number;
  images: CvmImage[];
  network_sources: CvmNetworkSource[];
}
export interface CvmChainOption {
  id: string;
  app_id: string;
  app_name: string;
  name: string;
  country: string | null;
  machines: string[];
  authorized: boolean;
  unavailable_reason?: string | null;
  used_by: string[];
  ipv6: boolean;
  reachable: boolean | null;
  rtt_ms: number | null;
}
export interface CvmChainCatalog {
  owner: { tenant_id: string; user_id: string; name: string } | null;
  supported: boolean;
  reason: string | null;
  chains: CvmChainOption[];
  hidden_unauthorized: number;
  traffic: UserTrafficPolicyView[];
}
export const fetchCvmChainCatalog = (instance?: string) =>
  api<CvmChainCatalog>(`/cvm/chain-egress${instance ? `?instance_id=${encodeURIComponent(instance)}` : ''}`);
export interface CvmResources {
  vcpus: number;
  memory_bytes: number;
  root_disk_bytes: number;
}
export interface CvmResourceLimits {
  min_vcpus: number;
  max_vcpus: number;
  min_memory_bytes: number;
  max_memory_bytes: number;
  memory_step_bytes: number;
  min_root_disk_bytes: number;
  max_root_disk_bytes: number;
  root_disk_step_bytes: number;
}
export interface CvmHost {
  host_revision: number;
  can_manage_host: boolean;
  terminal_enabled: boolean;
  terminal_revision: number;
  can_manage_terminal: boolean;
  terminal_sessions?: { used: number; limit: number };
  node_id: string;
  name: string;
  fresh: boolean;
  observed_at: string;
  capabilities: CvmCapabilities;
  error: string | null;
}
export interface CvmInterfaceRequest {
  source_id: string;
  purpose: 'contact' | 'business';
  default_ipv4: boolean;
  default_ipv6: boolean;
}
export interface CvmInterface extends CvmInterfaceRequest {
  id: string;
  allocated?: CvmAllocatedNic;
}
export type CvmStatus = 'pending' | 'applied' | 'failed' | 'unknown';
export type CvmPower = 'running' | 'stopped' | 'deleted';
export interface CvmInstance {
  revision_id?: number | null;
  published_revision_id?: number | null;
  blocking_deployment_id?: number | null;
  operation_kind?: string | null;
  tenant_id: string;
  user_id: string | null;
  node_id: string;
  reserved: boolean;
  created_at: string;
  desired: {
    id: string;
    operation_id: string;
    generation: number;
    spec_sha256: string;
    power: CvmPower;
    delete_data: boolean;
    restart_epoch: number;
    reinstall_epoch: number;
    spec: {
      routing: CvmContainerRoutes;
      name: string;
      image?: CvmImage;
      execution: { kind: 'cube' } | { kind: 'baremetal'; identity: CvmHostIdentity };
      resources: CvmResources;
      login_revision: number;
      interfaces: CvmInterface[];
    };
  };
  status: CvmStatus;
  report: null | {
    generation: number;
    operation_id: string;
    artifacts?: CvmArtifactReceipt;
    status: CvmStatus;
    power: CvmPower | null;
    runtime_id: string | null;
    volume_id: string | null;
    access_address: string | null;
    interfaces: { id: string; device: string; mac: string; addresses: string[]; attached: boolean }[];
    detached_interfaces: string[];
    network_absent: boolean;
    volume_retained: boolean;
    volume_deleted: boolean;
    error: string | null;
  };
  updated_at: string;
}
export interface CvmWorkspace {
  can_publish?: boolean;
  pending_revision_id?: number | null;
  available_hosts?: CvmHost[];
  can_manage_network: boolean;
  hosts: CvmHost[];
  instances: CvmInstance[];
  owners: { tenant_id: string; user_id: string; name: string }[];
  can_use: boolean;
  tenants?: { tenant_id: string; name: string }[];
  can_write: boolean;
  redacted: boolean;
}
export interface CvmCreate {
  request_id: string;
  tenant_id: string;
  user_id: string | null;
  node_id: string;
  name: string;
  image_id: string;
  resources: CvmResources;
  interfaces: CvmInterfaceRequest[];
}
export type CvmChange =
  | { action: 'start' | 'stop' | 'restart' | 'retry' | 'release_host' }
  | { action: 'undo_pending'; expected_revision: number }
  | { action: 'reinstall'; image_id: string }
  | { action: 'assign'; user_id: string | null }
  | { action: 'reset_password' }
  | { action: 'configure'; name: string; vcpus: number; memory_bytes: number }
  | { action: 'delete'; delete_data: boolean }
  | { action: 'detach'; interface_id: string };
export interface CvmPendingReview {
  revision: number;
  author: string | null;
  saved_at: string;
  fields: string[];
  entries_added: number;
  entries_removed: number;
  nics_added: number;
  nics_removed: number;
}
export const fetchCvmPendingReview = (id: string) => api<CvmPendingReview>(`/cvm/${encodeURIComponent(id)}/pending`);
export const fetchCvm = () => api<CvmWorkspace>('/cvm');
export const fetchCvmHostInstallCommand = () => api<{ install_command: string }>('/cvm/host-install-command');
export interface CvmCardSample {
  observed_at_unix_millis: number;
  window_start_unix_millis: number;
  has_gap: boolean;
  cpu_usage_basis_points: number | null;
  memory_total_bytes: number;
  memory_used_bytes: number;
  root_total_bytes: number;
  root_used_bytes: number;
  rx_bps: number | null;
  tx_bps: number | null;
}
export interface CvmCardReading {
  id: string;
  state: 'live' | 'stale' | 'unavailable' | 'stopped';
  latest: CvmCardSample | null;
  samples: CvmCardSample[];
  month_used_bytes: number | null;
  month_limit_bytes: number | null;
  month_coverage_gap: boolean;
  month_observed_at_unix_millis: number | null;
}
export interface CvmReadings {
  observed_at_unix_millis: number;
  instances: CvmCardReading[];
}
export const fetchCvmReadings = () => api<CvmReadings>('/cvm/readings');
export interface CvmCpuReading {
  cpu: number;
  user: number;
  system: number;
  softirq: number;
  iowait: number;
  steal: number;
}
export interface CvmDiskReading {
  read_bps: number | null;
  write_bps: number | null;
  read_iops: number | null;
  write_iops: number | null;
  read_await_ms: number | null;
  write_await_ms: number | null;
  busy_pct: number | null;
  queue_depth: number | null;
  in_flight: number;
}
export interface CvmDiagnosticReading {
  cpus: CvmCpuReading[];
  load: [number, number, number] | null;
  running_processes: number | null;
  total_processes: number | null;
  memory: Record<string, number>;
  swap_used_bytes: number | null;
  oom_kills: number | null;
  major_faults: number | null;
  pressure: { resource: string; some_basis_points: number; full_basis_points: number | null }[];
  inode_total: number | null;
  inode_free: number | null;
  disk_io: CvmDiskReading | null;
  tcp: Record<string, number>;
  unsupported: string[];
}
export interface CvmMonitoringSample {
  observed_at_unix_millis: number;
  window_start_unix_millis: number;
  uptime_millis: number;
  cpu_usage_basis_points: number | null;
  cpu_peak_basis_points: number | null;
  memory_available_min_bytes: number | null;
  has_gap: boolean;
  memory_total_bytes: number;
  memory_used_bytes: number;
  root_total_bytes: number;
  root_used_bytes: number;
  diagnostics: CvmDiagnosticReading | null;
  interfaces: {
    id: string;
    device: string;
    rx_bps: number | null;
    tx_bps: number | null;
    rx_pps: number | null;
    tx_pps: number | null;
    drops: number | null;
    errors: number | null;
    carrier: boolean | null;
  }[];
}
export interface CvmObservationHealth {
  id: string;
  generation: number;
  observed_at_unix_millis: number;
  runtime_id: string | null;
  runtime_state: 'unknown' | 'running' | 'stopped' | 'missing' | 'transitioning';
  guest_state: 'unknown' | 'reachable' | 'unreachable';
  ssh_state: 'unknown' | 'reachable' | 'unreachable';
  error: string | null;
  services: { id: string; state: 'unknown' | 'reachable' | 'unreachable'; latency_millis: number | null }[];
}
export interface CvmMonitoring {
  state: 'live' | 'stale' | 'unavailable' | 'stopped';
  error: string | null;
  range_start_unix_millis: number;
  range_end_unix_millis: number;
  samples: CvmMonitoringSample[];
  latest: CvmMonitoringSample | null;
  health: CvmObservationHealth | null;
  samples_truncated: boolean;
  issues: { kind: string; severity: string; message: string }[];
  events: { at_unix_millis: number; kind: string; detail: string }[];
}
export const fetchCvmMonitoring = (id: string, seconds = 3600) =>
  api<CvmMonitoring>(`/cvm/${encodeURIComponent(id)}/monitoring${seconds === 3600 ? '' : `?window_secs=${seconds}`}`);
export const createCvm = (body: CvmCreate) =>
  api<CvmInstance>('/cvm', '', { method: 'POST', body: JSON.stringify(body) });
export const commandCvm = (
  id: string,
  expectedGeneration: number,
  change: CvmChange,
  requestId: string,
  inputSha256?: string,
) =>
  api<CvmInstance>(`/cvm/${encodeURIComponent(id)}/operations`, '', {
    method: 'POST',
    body: JSON.stringify({ request_id: requestId, expected_generation: expectedGeneration, ...change }),
    ...(inputSha256 ? { headers: { 'x-brocade-cvm-input-sha256': inputSha256 } } : {}),
  });
export interface CvmNetworkQuotaCapacity {
  physical_memory_bytes: number;
  host_reserved_memory_bytes: number;
  contract: string;
  total_states: number;
  system_reserved_states: number;
  memory_budget_bytes: number;
  fixed_memory_bytes: number;
  bytes_per_state: number;
  model: string;
}
export const fetchCvmCredential = (id: string) =>
  api<{ instance_id: string; revision: number; password: string }>(`/cvm/${encodeURIComponent(id)}/credential`);

export interface CvmTerminalCredentials {
  session_id: string;
  ticket: string;
  expires_in_seconds: number;
  max_sessions_per_node: number;
  history_bytes: number;
  webrtc?: { supported: boolean; stun_servers: string[]; trickle?: boolean };
  latency_supported?: boolean;
}
export interface CvmTerminalSession {
  id: string;
  running: boolean;
  history_bytes: number;
  history_truncated: boolean;
}
export const createCvmTerminal = (
  id: string,
  signal?: AbortSignal,
  request: { terminal_id?: string; new_session?: boolean } = {},
) =>
  api<CvmTerminalCredentials>(`/cvm/${encodeURIComponent(id)}/terminal`, '', {
    method: 'POST',
    body: JSON.stringify(request),
    signal,
  });

export const setCvmTerminalPolicy = (node: string, enabled: boolean, expectedRevision: number) =>
  api<CvmWorkspace>(`/cvm/hosts/${encodeURIComponent(node)}/terminal`, '', {
    method: 'PUT',
    body: JSON.stringify({ enabled, expected_revision: expectedRevision }),
  });

export const setCvmHostPolicy = (node: string, enabled: boolean, expectedRevision: number) =>
  api<CvmWorkspace>(`/cvm/hosts/${encodeURIComponent(node)}`, '', {
    method: 'PUT',
    body: JSON.stringify({ enabled, expected_revision: expectedRevision }),
  });

export type CvmArtifactKind = 'cube/sandbox.json' | 'cube/egress.json' | 'cube/attachments.json' | 'guest/init.json';
export interface CvmArtifact {
  kind: CvmArtifactKind;
  sha256: string;
  content: string;
}
export interface CvmArtifactBundle {
  input?: {
    instance_id: string;
    spec: CvmInstance['desired']['spec'];
    image_manifest: { sha256: string; content: string };
    capabilities: CvmCapabilities;
  };
  compiler: string;
  input_sha256: string;
  spec_sha256: string;
  files: CvmArtifact[];
}
export interface CvmArtifactReceipt {
  compiler: string;
  input_sha256: string;
  mismatches: CvmArtifactKind[];
  files: { kind: CvmArtifactKind; compiled_sha256: string; effective_sha256: string | null; bindings: unknown }[];
}
export interface CvmArtifactHistory {
  generation: number;
  current: CvmArtifactBundle | null;
  previous: CvmArtifactBundle | null;
}
export const getCvmArtifacts = (id: string, generation?: number) =>
  api<CvmArtifactHistory>(`/cvm/${encodeURIComponent(id)}/artifacts${generation ? `?generation=${generation}` : ''}`);

export type CvmCompilePreviewRequest =
  | { kind: 'command'; request: { request_id: string; expected_generation: number } & CvmChange }
  | { kind: 'mount'; request: Parameters<typeof import('./cvm-nics').mountCvmNic>[1] }
  | { kind: 'routes'; request: Parameters<typeof import('./cvm-nics').setCvmRoutes>[1] };
export interface CvmCompilePreview {
  instance: CvmInstance;
  artifacts: CvmArtifactHistory | null;
  no_op: boolean;
}
export const previewCvmCompile = (id: string, request: CvmCompilePreviewRequest) =>
  api<CvmCompilePreview>(`/cvm/${encodeURIComponent(id)}/artifacts/preview`, '', {
    method: 'POST',
    body: JSON.stringify(request),
  });
