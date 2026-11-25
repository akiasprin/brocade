import { useQuery } from '@tanstack/react-query';
import { verifyDeployment } from '../api';
import { useRevisionDiff } from './artifacts';

/** New machines may already be initialized without advancing the business release baseline. */
export function useConfigurationStatus(revision: number | undefined, released: number | null | undefined) {
  const compare = revision != null && released !== undefined && revision !== released;
  const diff = useRevisionDiff(compare ? revision : undefined, compare ? released : undefined);
  // CVM changes have no artifact diff at all. Grant and subscription revisions also leave
  // configuration files unchanged, but their artifact diff must stay on the lightweight path.
  const verifyPlan =
    compare &&
    (diff.configurationOnlyNewNodes || (diff.known && diff.configurationNodes?.size === 0 && diff.changed.size === 0));
  const verification = useQuery({
    queryKey: ['deployment-verify', revision],
    queryFn: () => verifyDeployment({ revision_id: revision! }),
    enabled: verifyPlan,
    staleTime: 5_000,
    refetchInterval: query => ((query.state.data?.summary.changed_targets ?? 0) > 0 ? 5_000 : false),
  });
  const pendingChanges =
    revision == null || released === undefined
      ? undefined
      : revision === released
        ? false
        : verifyPlan
          ? verification.error || !verification.data
            ? undefined
            : verification.data.summary.changed_targets > 0
          : diff.configurationNodes == null
            ? undefined
            : diff.configurationNodes.size > 0;

  return {
    pendingChanges,
    pending: compare && (diff.pending || (verifyPlan && verification.isPending)),
    error: compare ? (diff.error ?? (verifyPlan ? verification.error : null)) : null,
  };
}
