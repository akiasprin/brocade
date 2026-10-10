import { useCallback, useEffect, useId } from 'react';

interface UnsavedEntry {
  label: string;
  scope?: string;
  preserveOnLeave?: () => void;
}

const entries = new Map<string, UnsavedEntry>();

const activeEntries = (scope?: string): Array<[string, UnsavedEntry]> =>
  [...entries.entries()].filter(([, entry]) => scope === undefined || entry.scope === scope);

/**
 * Some editors can synchronously promote their local state into the persistent browser draft.
 * Navigation never waits for or prompts about local edits, but it still gives those editors one
 * synchronous chance to preserve their state before their surface unmounts.
 */
const preserveEntries = (scope?: string) => {
  for (const [id, entry] of activeEntries(scope)) {
    if (!entry.preserveOnLeave) continue;
    try {
      entry.preserveOnLeave();
      entries.delete(id);
    } catch {
      // Keep guarding local state when it could not be persisted.
    }
  }
};

/**
 * Give every editor its normal synchronous persistence chance, then report whether a document
 * reload is lossless. Unlike in-app navigation, a reload destroys component memory, so entries
 * without a persistence callback must keep the old document alive until the user saves or closes
 * them.
 */
export function prepareForDocumentReload(): boolean {
  preserveEntries();
  return entries.size === 0;
}

/**
 * Register local form state that would be discarded when its owning surface unmounts.
 * The returned callback synchronously marks this entry saved, so a successful mutation may
 * navigate in the same tick without being mistaken for an attempted discard.
 */
export function useUnsavedChanges(
  active: boolean,
  label: string,
  scope?: string,
  preserveOnLeave?: () => void,
): () => void {
  const id = useId();
  const clear = useCallback(() => {
    entries.delete(id);
  }, [id]);

  useEffect(() => {
    if (active) entries.set(id, { label, scope, preserveOnLeave });
    else entries.delete(id);

    return clear;
  }, [active, clear, id, label, preserveOnLeave, scope]);

  return clear;
}

/** Preserve what can be preserved and always allow navigation, even when local edits will be lost. */
export function confirmDiscardChanges(scope?: string): boolean {
  preserveEntries(scope);
  return true;
}
