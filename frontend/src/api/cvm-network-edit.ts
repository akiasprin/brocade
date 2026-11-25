import { api } from './client';
import type { CvmInstance } from './cvm';
import type { CvmEntryMode } from './cvm-entries';
import type { CvmContainerRoutes, CvmNicTransport } from './cvm-nics';

export interface CvmNetworkEditRequest {
  request_id: string;
  expected_generation: number;
  mounts?: {
    /** Also accepted as interface_id in this edit's routes and priorities. */
    draft_id: string;
    publication_id: string;
    transport: CvmNicTransport;
    use_dns_suggestions?: boolean;
  }[];
  detach_interface_ids?: string[];
  entry_additions?: { publication_id: string; address: string; mode: CvmEntryMode }[];
  entry_releases?: { id: string; expected_revision: number }[];
  /** The complete final table. When omitted, mount defaults apply and detached targets are removed. */
  routes?: { expected_revision: number; routes: CvmContainerRoutes };
}

export interface CvmNetworkEditResult {
  instance: CvmInstance;
  mounted_interfaces: Record<string, string>;
  input_sha256: string;
}

export const saveCvmNetworkEdit = (id: string, body: CvmNetworkEditRequest, inputSha256?: string) =>
  api<CvmNetworkEditResult>(`/cvm/${encodeURIComponent(id)}/network-edit`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
    ...(inputSha256 ? { headers: { 'x-brocade-cvm-input-sha256': inputSha256 } } : {}),
  });

export const previewCvmNetworkEdit = (id: string, body: CvmNetworkEditRequest) =>
  api<CvmNetworkEditResult>(`/cvm/${encodeURIComponent(id)}/network-edit?preview=true`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });
