import { useQuery } from '@tanstack/react-query';
import { verifyDeployment } from '../api';
import { useRevisionDiff } from './artifacts';

/** New machines may already be initialized without advancing the business release baseline. */
export function useConfigurationStatus(revision: number | undefined, released: number | null | undefined) {
  const compare = revision != null && released !== undefined && revision !== released;
  const diff = useRevisionDiff(compare ? revision : undefined, compare ? released : undefined);
  // Keep ordinary edits on the lightweight index path. For new machines, share the release
  // page's live check rather than mistaking a missing historical file for an unapplied one.
  const verifyNewNodes = compare && diff.configurationOnlyNewNodes;
  const verification = useQuery({
    queryKey: ['deployment-verify', revision],
    queryFn: () => verifyDeployment({ revision_id: revision! }),
    enabled: verifyNewNodes,
    staleTime: 5_000,
    refetchInterval: query => ((query.state.data?.summary.changed_targets ?? 0) > 0 ? 5_000 : false),
  });
  const pendingChanges =
    revision == null || released === undefined
      ? undefined
      : revision === released
        ? false
        : verifyNewNodes
          ? verification.error || !verification.data
            ? undefined
            : verification.data.summary.changed_targets > 0
          : diff.configurationNodes == null
            ? undefined
            : diff.configurationNodes.size > 0;

  return {
    pendingChanges,
    pending: compare && (diff.pending || (verifyNewNodes && verification.isPending)),
    error: compare ? (diff.error ?? (verifyNewNodes ? verification.error : null)) : null,
  };
}
