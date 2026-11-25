import { skipToken, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useCallback, useSyncExternalStore } from 'react';
import type { CvmInstance, CvmWorkspace } from '../../api';
import {
  previewCvmNetworkEdit,
  saveCvmNetworkEdit,
  type CvmNetworkEditRequest,
  type CvmNetworkEditResult,
} from '../../api/cvm-network-edit';
import { useUnsavedChanges } from '../../ui/navigation-guard';
import type { CreateNetworkTemplate } from './create';
import { uncertainCvmResult } from './network-common';
import { invalidateCvmChanges, QUERY, requestId } from './queries';

export type NetworkEdits = Omit<CvmNetworkEditRequest, 'request_id' | 'expected_generation'>;

interface Draft {
  instance: CvmInstance;
  request: CvmNetworkEditRequest;
  review?: CvmNetworkEditResult;
  uncertain?: boolean;
}

const DRAFTS = ['cvm-network-drafts'] as const;
const EMPTY: Record<string, Draft> = {};

export function editCount(edits: NetworkEdits) {
  return (
    (edits.entry_additions?.length ?? 0) +
    (edits.entry_releases?.length ?? 0) +
    (edits.mounts?.length ?? 0) +
    (edits.detach_interface_ids?.length ?? 0) +
    Number(!!edits.routes)
  );
}

function newDraft(instance: CvmInstance, edits: NetworkEdits): Draft {
  return {
    instance,
    request: { ...edits, request_id: requestId(), expected_generation: instance.desired.generation },
  };
}

export function useNetworkDraft(instance: CvmInstance | undefined, writable: boolean) {
  const client = useQueryClient();
  // Keep the in-session cache entry alive and deliver edits synchronously to
  // controlled fields. A deferred Query notification can leave the next input
  // handler reading the preceding route table and reverting a sibling edit.
  useQuery({ queryKey: DRAFTS, queryFn: skipToken, initialData: EMPTY, gcTime: Infinity, notifyOnChangeProps: [] });
  const cache = client.getQueryCache();
  const subscribe = useCallback(
    (notify: () => void) =>
      cache.subscribe(event => {
        if (event.query.queryKey[0] === DRAFTS[0] && (event.type === 'updated' || event.type === 'removed')) notify();
      }),
    [cache],
  );
  const getDrafts = useCallback(() => client.getQueryData<Record<string, Draft>>(DRAFTS) ?? EMPTY, [client]);
  const drafts = useSyncExternalStore(subscribe, getDrafts, getDrafts);
  const id = instance?.desired.id;
  const draft = writable && id ? drafts[id] : undefined;
  const others = writable ? Object.values(drafts).filter(other => other.instance.desired.id !== id) : [];
  useUnsavedChanges(writable && Object.keys(drafts).length > 0, '容器网络草稿');

  function set(target: string, update: Draft | undefined) {
    client.setQueryData<Record<string, Draft>>(DRAFTS, previous => {
      const next = { ...previous };
      if (update) next[target] = update;
      else delete next[target];
      return next;
    });
  }

  const mutation = useMutation({
    mutationFn: async ({ draft, save }: { draft: Draft; save: boolean }) => {
      const id = draft.instance.desired.id;
      let review = draft.review;
      if (!review) {
        review = await previewCvmNetworkEdit(id, draft.request);
        const current = client.getQueryData<Record<string, Draft>>(DRAFTS)?.[id];
        if (current?.request.request_id !== draft.request.request_id) throw new Error('草稿已变化，请重新检查。');
        set(id, { ...draft, review });
      }
      if (save) return saveCvmNetworkEdit(id, draft.request, review.input_sha256 ?? undefined);
      return review;
    },
    onSuccess: async (result, { draft, save }) => {
      if (!save) return;
      await client.cancelQueries({ queryKey: QUERY, exact: true });
      const id = draft.instance.desired.id;
      if (client.getQueryData<Record<string, Draft>>(DRAFTS)?.[id]?.request.request_id !== draft.request.request_id)
        return;
      client.setQueryData<CvmWorkspace>(
        QUERY,
        old =>
          old && {
            ...old,
            pending_revision_id: Math.max(old.pending_revision_id ?? 0, result.instance.revision_id ?? 0) || null,
            instances: old.instances.map(item => (item.desired.id === id ? result.instance : item)),
          },
      );
      set(id, undefined);
      invalidateCvmChanges(client);
    },
    onError: (error, { draft, save }) => {
      const id = draft.instance.desired.id;
      const current = client.getQueryData<Record<string, Draft>>(DRAFTS)?.[id];
      if (current?.request.request_id !== draft.request.request_id) return;
      if (save && current.review) {
        set(
          id,
          uncertainCvmResult(error)
            ? { ...current, uncertain: true }
            : { ...current, review: undefined, uncertain: false },
        );
      }
      void client.invalidateQueries({ queryKey: QUERY });
    },
  });
  const busy = mutation.isPending && mutation.variables?.draft.instance.desired.id === id;
  const error = mutation.variables?.draft.request.request_id === draft?.request.request_id ? mutation.error : null;

  return {
    draft,
    others,
    busy,
    error,
    stale: !!draft && draft.request.expected_generation !== instance?.desired.generation && !draft.uncertain,
    change: (edits: NetworkEdits) => {
      if (!id || !instance || !writable || busy || draft?.uncertain) return;
      const { entry_additions, entry_releases, mounts, detach_interface_ids, routes } = edits;
      const fields = { entry_additions, entry_releases, mounts, detach_interface_ids, routes };
      set(id, editCount(fields) ? newDraft(draft?.instance ?? instance, fields) : undefined);
    },
    seed: (instance: CvmInstance, network: CreateNetworkTemplate) => {
      const fields: NetworkEdits = {
        entry_additions: network.entry ? [network.entry] : [],
        mounts: network.nics.map(nic => ({ ...nic, draft_id: `draft-${requestId()}` })),
      };
      if (editCount(fields)) set(instance.desired.id, newDraft(instance, fields));
    },
    discard: () => {
      if (id && !busy && !draft?.uncertain) set(id, undefined);
    },
    submit: (save: boolean) => {
      if (draft && !mutation.isPending) mutation.mutate({ draft, save });
    },
  };
}

export type NetworkDraftController = ReturnType<typeof useNetworkDraft>;
