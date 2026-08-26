import { ApiError, api, type Dns, type ProvisionNodeResult } from '../api';

export interface PreviewStatus {
  enabled: boolean;
  runtime: 'docker';
  console_url: string;
  agent_url: string;
  preview_public_url: string;
  network: string;
  subnet: string;
  subnet_ipv6: string;
  node_image: string;
  agent_binary_ready: boolean;
}

export interface PreviewRuntime {
  container_name: string;
  /** Backward-compatible alias for IPv4. */
  ip: string;
  ipv4: string;
  ipv6: string;
  network: string;
  image: string;
  install_started: boolean;
  install_log_path: string;
}

export interface PreviewProvisionNodeResult extends ProvisionNodeResult {
  preview: PreviewRuntime;
}

export interface PreviewProvisionNodeRequest {
  id: string;
  tenant_id: string;
  name?: string | null;
  public_ipv4_nat?: boolean;
  public_ipv6_nat?: boolean;
  egress_allowed?: boolean;
  dns?: Dns;
}

export interface PreviewNodeLogs {
  node_id: string;
  container_name: string;
  install_log: string;
  container_log: string;
}

export interface PreviewVerifySubscriptionResult {
  user: string;
  ok: boolean;
  stage: string;
  message: string;
  entries: Array<{
    name: string;
    uri_parse: string;
    connect: string;
    http: string;
    stats_changed: boolean;
    stats_scope: string;
    stats_container?: string | null;
    stats_label_prefix?: string | null;
    stats_before?: number | null;
    stats_after?: number | null;
    stats_labels_after?: Record<string, number> | null;
    stats_error?: string | null;
    egress_ip?: string | null;
    log?: string;
    error?: string;
  }>;
}

export async function fetchPreviewStatus(): Promise<PreviewStatus> {
  try {
    return await api<PreviewStatus>('/preview/status');
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      return {
        enabled: false,
        runtime: 'docker',
        console_url: '',
        agent_url: '',
        preview_public_url: '',
        network: '',
        subnet: '',
        subnet_ipv6: '',
        node_image: '',
        agent_binary_ready: false,
      };
    }
    throw error;
  }
}

export const previewProvisionNode = (body: PreviewProvisionNodeRequest) =>
  api<PreviewProvisionNodeResult>('/preview/nodes', '', {
    method: 'POST',
    body: JSON.stringify(body),
  });

export const fetchPreviewNodeLogs = (nodeId: string) =>
  api<PreviewNodeLogs>(`/preview/nodes/${encodeURIComponent(nodeId)}/logs`);

export const previewVerifySubscription = (body: { tenant_id: string; user_id: string }) =>
  api<PreviewVerifySubscriptionResult>('/preview/verify/subscription', '', {
    method: 'POST',
    body: JSON.stringify(body),
  });
