import { useQuery } from '@tanstack/react-query';
import { verifyDeployment } from '../api';

/** A successful release baseline records coverage, while live verification records convergence. */
export function useConfigurationStatus(revision: number | undefined, released: number | null | undefined) {
  const compare = revision != null && released !== undefined && revision !== released;
  const verification = useQuery({
    queryKey: ['deployment-verify', revision],
    queryFn: () => verifyDeployment({ revision_id: revision! }),
    enabled: compare,
    staleTime: 5_000,
    refetchInterval: query => ((query.state.data?.summary.changed_targets ?? 0) > 0 ? 5_000 : false),
  });
  const pendingChanges =
    revision == null || released === undefined
      ? undefined
      : revision === released
        ? false
        : verification.error || !verification.data
          ? undefined
          : verification.data.summary.changed_targets > 0;

  return {
    pendingChanges,
    pending: compare && verification.isPending,
    error: compare ? verification.error : null,
  };
}
