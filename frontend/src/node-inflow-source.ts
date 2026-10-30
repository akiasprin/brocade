import type { NodeInflowRow } from './api';

/** Chains keep their identity when moved; app_id is historical billing attribution. */
export function inflowSourceKey(row: Pick<NodeInflowRow, 'source' | 'app_id' | 'chain_id' | 'ingress_id'>): string {
  if (row.source !== 'chain') return row.source;
  return row.chain_id ? JSON.stringify([null, row.chain_id, null]) : JSON.stringify([row.app_id, null, row.ingress_id]);
}
