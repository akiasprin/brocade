import { api } from './client';
import type { CvmInstance } from './cvm';

export type CvmNicTransport = 'xray' | 'wireguard';
export type CvmNicAddressMode = 'shared_nat' | 'dedicated_routed' | 'dedicated_nat';
export interface CvmNicLimits {
  max_attachments: number;
  upload_bps: number | null;
  download_bps: number | null;
  monthly_bytes: number | null;
}
export interface CvmTransportOffer {
  listen_side?: 'auto' | 'provider' | 'consumer';
  kind: CvmNicTransport;
  endpoint: string;
  port_start: number;
  port_end: number;
  mtu: number;
}
export interface CvmNicPublicationSpec {
  name: string;
  provider_node: string;
  resource_domain: string;
  ipv4_pool: string | null;
  ipv6_pool: string | null;
  public_ipv4_pool: string | null;
  public_ipv6_pool: string | null;
  shared_ipv4: string | null;
  shared_ipv6: string | null;
  address_mode: CvmNicAddressMode;
  /** 出口网卡可达的目标网段；入站由访问入口承担。 */
  destinations: string[];
  route_presets: string[];
  dns_suggestions: string[];
  transports: CvmTransportOffer[];
  grants: { tenant_id: string; user_id: string }[];
  limits: CvmNicLimits;
}
export interface CvmNicPublication {
  id: string;
  revision: number;
  admitting: boolean;
  retired: boolean;
  spec: CvmNicPublicationSpec;
}
export interface CvmAllocatedNic {
  slot: number;
  publication_id: string;
  publication_revision: number;
  provider_node: string;
  resource_domain: string;
  allocation_id: string;
  mac: string;
  ipv4: string | null;
  ipv6: string | null;
  public_ipv4: string | null;
  public_ipv6: string | null;
  address_mode: CvmNicAddressMode;
  destinations: string[];
  transport: CvmNicTransport;
  transport_generation: number;
  mtu: number;
  blocked: boolean;
  quota_exceeded: boolean;
  limits: CvmNicLimits;
}
/** L4 出口：协议加目的端口段。路由不带它时只按目标网段匹配（L3）。 */
export interface CvmRouteService {
  protocol: 'tcp' | 'udp';
  port_start: number;
  port_end: number;
}
export interface CvmContainerRoute {
  destination: string;
  interface_id: string;
  service?: CvmRouteService | null;
}
export interface CvmContainerRoutes {
  revision: number;
  priorities: { interface_id: string; ipv4: number | null; ipv6: number | null }[];
  routes: CvmContainerRoute[];
  dns: string[];
}
export interface CvmNicNodeCapabilities {
  contract: number;
  transports: CvmNicTransport[];
  consume: boolean;
  provide: boolean;
  max_bindings: number;
  addresses: string[];
  host_ports: { protocol: 'tcp' | 'udp'; port: number }[];
  error: string | null;
  /** Agent 能执行访问入口（规则、宿主投递、机器对通道）。 */
  entries?: boolean;
  /** Agent 能在宿主侧执行按协议端口选择的出口。 */
  route_service?: boolean;
}
export interface CvmNicAttachment {
  allocation_id: string;
  interface_id: string;
  instance_id: string;
  instance_name: string;
  publication_id: string;
  publication_revision: number;
  generation: number;
  attached: boolean;
  route_references: number;
  cleanup_pending: boolean;
}
export interface CvmNicCatalog {
  attachments: CvmNicAttachment[];
  publications: CvmNicPublication[];
  providers: { node_id: string; name: string; fresh: boolean; capabilities: CvmNicNodeCapabilities }[];
  can_manage: boolean;
}
export type CvmBindingPhase = 'preparing' | 'active' | 'standby' | 'withdrawing';
export interface CvmNicBindingReport {
  local_block?: string | null;
  id: string;
  generation: number;
  side: 'consumer' | 'provider';
  phase: CvmBindingPhase;
  configured: boolean;
  reachable: boolean | null;
  rx_bytes: number;
  tx_bytes: number;
  error: string | null;
}
export interface CvmNicState {
  recent_errors?: CvmNicState['bindings'];
  usage: Record<
    string,
    { period: string; uploaded_bytes: number; downloaded_bytes: number; coverage_gap: boolean; observed_at: number }
  >;
  mount_options: Record<string, { transports: CvmNicTransport[]; error: string | null }>;
  interface_options: Record<string, { transports: CvmNicTransport[]; error: string | null }>;
  bindings: {
    consumer_fresh: boolean;
    provider_fresh: boolean;
    id: string;
    interface_id: string;
    transport: CvmNicTransport;
    generation: number;
    phase: CvmBindingPhase;
    consumer: CvmNicBindingReport | null;
    provider: CvmNicBindingReport | null;
    error: string | null;
  }[];
}

export const fetchCvmNicCatalog = () => api<CvmNicCatalog>('/cvm/networks');
export const writeCvmNicPublication = (
  id: string | null,
  body: { request_id: string; expected_revision: number; spec: CvmNicPublicationSpec },
) =>
  api<CvmNicPublication>(id ? `/cvm/networks/${encodeURIComponent(id)}` : '/cvm/networks', '', {
    method: id ? 'PUT' : 'POST',
    body: JSON.stringify(body),
  });
export const setCvmNicAdmission = (
  id: string,
  body: { request_id: string; expected_revision: number; admitting: boolean },
) =>
  api<CvmNicPublication>(`/cvm/networks/${encodeURIComponent(id)}/admission`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });
export const deleteCvmNicPublication = (
  id: string,
  body: { request_id: string; expected_revision: number; admitting: boolean },
) =>
  api<CvmNicPublication>(`/cvm/networks/${encodeURIComponent(id)}`, '', {
    method: 'DELETE',
    body: JSON.stringify(body),
  });
export const fetchCvmNicState = (id: string) => api<CvmNicState>(`/cvm/${encodeURIComponent(id)}/network`);
export const mountCvmNic = (
  id: string,
  body: {
    request_id: string;
    expected_generation: number;
    publication_id: string;
    transport: CvmNicTransport;
    use_dns_suggestions?: boolean;
  },
) => api<CvmInstance>(`/cvm/${encodeURIComponent(id)}/interfaces`, '', { method: 'POST', body: JSON.stringify(body) });
export const setCvmRoutes = (
  id: string,
  body: { request_id: string; expected_generation: number; expected_revision: number; routes: CvmContainerRoutes },
) => api<CvmInstance>(`/cvm/${encodeURIComponent(id)}/routes`, '', { method: 'PUT', body: JSON.stringify(body) });
export const changeCvmNicTransport = (
  id: string,
  nic: string,
  body: { request_id: string; expected_generation: number; transport: CvmNicTransport; rebuild?: boolean },
) =>
  api<CvmInstance>(`/cvm/${encodeURIComponent(id)}/interfaces/${encodeURIComponent(nic)}/transport`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });
export const updateCvmNicVersion = (
  id: string,
  nic: string,
  body: {
    request_id: string;
    expected_generation: number;
    publication_revision: number;
    reallocate: boolean;
    transport?: CvmNicTransport;
  },
) =>
  api<CvmInstance>(`/cvm/${encodeURIComponent(id)}/interfaces/${encodeURIComponent(nic)}/version`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });

export const updateCvmNicVersions = (
  id: string,
  body: {
    request_id: string;
    expected_generation: number;
    updates: { interface_id: string; publication_revision: number; reallocate: boolean; transport?: CvmNicTransport }[];
  },
) =>
  api<CvmInstance>(`/cvm/${encodeURIComponent(id)}/interface-versions`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });
