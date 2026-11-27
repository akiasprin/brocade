import { api } from './client';

export type StackStorageBackend = 'xfs' | 'plain';

export const STACK_COLD_PAIR_CONTRACT = 'brocade-cvm-stack-cold-pair-v1';
export const STACK_FRESH_INSTALL_CONTRACT = 'brocade-cvm-stack-fresh-install-v1';

export type StackReleaseStatus =
  'preparing' | 'ready' | 'applying' | 'succeeded' | 'failed' | 'recovered' | 'blocked' | 'cancelled';

export type StackReleasePair = {
  id: string;
  target_sha256: string;
  storage_backend: StackStorageBackend;
} & (
  | {
      contract: typeof STACK_COLD_PAIR_CONTRACT;
      source_sha256: string;
      source_cubelet_sha256: string;
      source_shim_sha256: string;
    }
  | {
      contract: typeof STACK_FRESH_INSTALL_CONTRACT;
      source_sha256?: null;
      source_cubelet_sha256?: null;
      source_shim_sha256?: null;
    }
);

export type StackReleaseHost = {
  node_id: string;
  observed_at: number;
} & (
  | { installable: StackStorageBackend }
  | {
      installable?: never;
      bundle_sha256: string;
      cubelet_sha256: string;
      shim_sha256: string;
      storage_backend: StackStorageBackend;
    }
);

export interface StackRelease {
  id: number;
  node_id: string;
  source_sha256: string | null;
  target_sha256: string;
  status: StackReleaseStatus;
  start_deadline_at: number;
  error?: string | null;
  roots_sha256?: string | null;
}

export interface StackReleaseView {
  pairs: StackReleasePair[];
  hosts: StackReleaseHost[];
  releases: StackRelease[];
}

export interface CreateStackRelease {
  node_id: string;
  pair_id: string;
}

const path = '/cvm/stack-releases';

export const fetchStackReleases = (signal?: AbortSignal) => api<StackReleaseView>(path, '', { signal });
export const fetchStackReleaseHistory = (signal?: AbortSignal) =>
  api<StackRelease[]>(`${path}/history`, '', { signal });
export const createStackRelease = (request: CreateStackRelease) =>
  api<StackRelease>(path, '', { method: 'POST', body: JSON.stringify(request) });
export const cancelStackRelease = (id: number) => api<StackRelease>(`${path}/${id}/cancel`, '', { method: 'POST' });
