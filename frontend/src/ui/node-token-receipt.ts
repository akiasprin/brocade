import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { issueNodeToken } from '../api';

type NodeTokenReceipt = Awaited<ReturnType<typeof issueNodeToken>>;
const receiptKey = (node: string) => ['node-token-receipt', node] as const;

/** One-time responses stay in session memory under their original machine, including late replies. */
export function useNodeTokenReceipt(node: string) {
  const client = useQueryClient();
  const receipt = useQuery<NodeTokenReceipt | null>({
    queryKey: receiptKey(node),
    queryFn: () => null,
    enabled: false,
    staleTime: Infinity,
    gcTime: 30 * 60_000,
  });
  return {
    receipt: receipt.data ?? null,
    receive: (result: NodeTokenReceipt) => client.setQueryData(receiptKey(result.node_id), result),
    dismiss: () => client.setQueryData(receiptKey(node), null),
  };
}
